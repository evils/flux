//! Bootstrap provides an API for compiling the Flux standard library.
//!
//! This package does not assume a location of the source code but does assume which packages are
//! part of the prelude.

use std::{env::consts, fs, io, io::Write, path::Path, sync::Arc};

use anyhow::{bail, Result};
use libflate::gzip::Encoder;
use walkdir::WalkDir;

use crate::{
    ast,
    semantic::{
        self,
        env::Environment,
        flatbuffers::types::{build_module, finish_serialize},
        fs::{FileSystemImporter, StdFS},
        import::{Importer, Packages},
        nodes::{self, Package, Symbol},
        sub::{Substitutable, Substituter},
        types::{
            BoundTvar, BoundTvarKinds, MonoType, PolyType, PolyTypeHashMap, Record, RecordLabel,
            SemanticMap, Tvar,
        },
        Analyzer, AnalyzerConfig, PackageExports,
    },
};

const INTERNAL_PRELUDE: [&str; 2] = ["internal/boolean", "internal/location"];

// List of packages to include into the Flux prelude
const PRELUDE: [&str; 4] = [
    "internal/boolean",
    "internal/location",
    "universe",
    "influxdata/influxdb",
];

/// A mapping of package import paths to the corresponding AST package.
pub type ASTPackageMap = SemanticMap<String, ast::Package>;
/// A mapping of package import paths to the corresponding semantic graph package.
pub type SemanticPackageMap = SemanticMap<String, Package>;

/// Infers the Flux standard library given the path to the source code.
/// The prelude and the imports are returned.
#[allow(clippy::type_complexity)]
pub fn infer_stdlib_dir(
    path: impl AsRef<Path>,
    config: AnalyzerConfig,
) -> Result<(PackageExports, Packages, SemanticPackageMap)> {
    infer_stdlib_dir_(path.as_ref(), config)
}

#[allow(clippy::type_complexity)]
fn infer_stdlib_dir_(
    path: &Path,
    config: AnalyzerConfig,
) -> Result<(PackageExports, Packages, SemanticPackageMap)> {
    let (mut db, package_list) = parse_dir(path)?;

    db.set_analyzer_config(config);

    let mut imports = Packages::default();
    let mut sem_pkg_map = SemanticPackageMap::default();
    for name in &package_list {
        let (exports, pkg) = db.semantic_package(name.clone())?;
        imports.insert(name.clone(), PackageExports::clone(&exports)); // TODO Clone Arc
        sem_pkg_map.insert(name.clone(), Package::clone(&pkg)); // TODO Clone Arc
    }

    let prelude = db.prelude()?;
    Ok((PackageExports::clone(&prelude), imports, sem_pkg_map))
}

/// Recursively parse all flux files within a directory.
pub fn parse_dir(dir: &Path) -> io::Result<(Database, Vec<String>)> {
    let mut db = Database::default();
    let mut package_names = Vec::new();
    let entries = WalkDir::new(dir)
        .into_iter()
        .filter_map(|r| r.ok())
        .filter(|r| r.path().is_file());

    let is_windows = consts::OS == "windows";

    for entry in entries {
        if let Some(path) = entry.path().to_str() {
            if path.ends_with(".flux") && !path.ends_with("_test.flux") {
                let mut normalized_path = path.to_string();
                if is_windows {
                    // When building on Windows, the paths generated by WalkDir will
                    // use `\` instead of `/` as their separator. It's easier to normalize
                    // the separators to always be `/` here than it is to change the
                    // rest of this buildscript & the flux runtime initialization logic
                    // to work with either separator.
                    normalized_path = normalized_path.replace('\\', "/");
                }
                let source = Arc::<str>::from(fs::read_to_string(entry.path())?);

                let file_name = normalized_path
                    .rsplitn(2, "/stdlib/")
                    .collect::<Vec<&str>>()[0]
                    .to_owned();
                let path = file_name.rsplitn(2, '/').collect::<Vec<&str>>()[1].to_string();
                package_names.push(path);
                db.set_source(file_name, source.clone());
            }
        }
    }

    Ok((db, package_names))
}

fn stdlib_importer(path: &Path) -> FileSystemImporter<StdFS> {
    let fs = StdFS::new(path);
    FileSystemImporter::new(fs)
}

fn prelude_from_importer<I>(importer: &mut I) -> Result<PackageExports>
where
    I: Importer,
{
    let mut env = PolyTypeHashMap::new();
    for pkg in PRELUDE {
        if let Ok(pkg_type) = importer.import(pkg) {
            if let MonoType::Record(typ) = pkg_type.expr {
                add_record_to_map(&mut env, typ.as_ref(), &pkg_type.vars, &pkg_type.cons)?;
            } else {
                bail!("package type is not a record");
            }
        } else {
            bail!("prelude package {} not found", pkg);
        }
    }
    let exports = PackageExports::try_from(env)?;
    Ok(exports)
}

// Collects any `MonoType::BoundVar`s in the type
struct CollectBoundVars(Vec<BoundTvar>);

impl Substituter for CollectBoundVars {
    fn try_apply(&mut self, _var: Tvar) -> Option<MonoType> {
        None
    }

    fn try_apply_bound(&mut self, var: BoundTvar) -> Option<MonoType> {
        let vars = &mut self.0;
        if let Err(i) = vars.binary_search(&var) {
            vars.insert(i, var);
        }
        None
    }
}

fn add_record_to_map(
    env: &mut PolyTypeHashMap<Symbol>,
    r: &Record,
    free_vars: &[BoundTvar],
    cons: &BoundTvarKinds,
) -> Result<()> {
    for field in r.fields() {
        let new_vars = {
            let mut new_vars = CollectBoundVars(Vec::new());
            field.v.visit(&mut new_vars);
            new_vars.0
        };

        let mut new_cons = BoundTvarKinds::new();
        for var in &new_vars {
            if !free_vars.iter().any(|v| v == var) {
                bail!("monotype contains free var not in poly type free vars");
            }
            if let Some(con) = cons.get(var) {
                new_cons.insert(*var, con.clone());
            }
        }
        env.insert(
            match &field.k {
                RecordLabel::Concrete(s) => s.clone().into(),
                RecordLabel::BoundVariable(_) | RecordLabel::Variable(_) => {
                    bail!("Record contains variable labels")
                }
                RecordLabel::Error => {
                    bail!("Record contains type error")
                }
            },
            PolyType {
                vars: new_vars,
                cons: new_cons,
                expr: field.v.clone(),
            },
        );
    }
    Ok(())
}

/// Stdlib returns the prelude and importer for the Flux standard library given a path to a
/// compiled directory structure.
pub fn stdlib(dir: &Path) -> Result<(PackageExports, FileSystemImporter<StdFS>)> {
    let mut stdlib_importer = stdlib_importer(dir);
    let prelude = prelude_from_importer(&mut stdlib_importer)?;
    Ok((prelude, stdlib_importer))
}

/// Compiles the stdlib found at the srcdir into the outdir.
pub fn compile_stdlib(srcdir: &Path, outdir: &Path) -> Result<()> {
    let (_, imports, mut sem_pkgs) = infer_stdlib_dir(srcdir, AnalyzerConfig::default())?;
    // Write each file as compiled module
    for (path, exports) in &imports {
        if let Some(code) = sem_pkgs.remove(path) {
            let module = Module {
                polytype: Some(exports.typ()),
                code: Some(code),
            };
            let mut builder = flatbuffers::FlatBufferBuilder::new();
            let offset = build_module(&mut builder, module);
            let buf = finish_serialize(&mut builder, offset);

            // Write module contents to file
            let mut fpath = outdir.join(path);
            fpath.set_extension("fc");
            fs::create_dir_all(fpath.parent().unwrap())?;
            let file = fs::File::create(&fpath)?;
            let mut encoder = Encoder::new(file)?;
            encoder.write_all(buf)?;
            encoder.finish().into_result()?;
        } else {
            bail!("package {} missing code", &path);
        }
    }
    Ok(())
}

/// Module represenets the result of compiling Flux source code.
///
/// The polytype represents the type of the entire package as a record type.
/// The record properties represent the exported values from the package.
///
/// The package is the actual code of the package that can be used to execute the package.
///
/// This struct is experimental we anticipate it will change as we build more systems around
/// the concepts of modules.
pub struct Module {
    /// The polytype
    pub polytype: Option<PolyType>,
    /// The code
    pub code: Option<nodes::Package>,
}

#[allow(missing_docs)] // Warns on the generated FluxStorage type
mod db {
    use crate::{
        errors::{located, SalvageResult},
        parser,
        semantic::{nodes, FileErrors, PackageExports},
    };

    use super::*;

    use std::{
        collections::HashSet,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Debug)]
    pub struct NeverEq<T>(pub T);

    impl<T> Eq for NeverEq<T> {}
    impl<T> PartialEq for NeverEq<T> {
        fn eq(&self, _: &Self) -> bool {
            false
        }
    }

    pub trait FluxBase {
        fn has_package(&self, package: &str) -> bool;
        fn package_files(&self, package: &str) -> Vec<String>;
        fn set_source(&mut self, path: String, source: Arc<str>);
        fn source(&self, path: String) -> Arc<str>;
    }

    /// Defines queries that drives flux compilation
    #[salsa::query_group(FluxStorage)]
    pub trait Flux: FluxBase {
        /// Source code for a particular flux file
        #[salsa::input]
        #[doc(hidden)]
        fn source_inner(&self, path: String) -> Arc<str>;

        #[salsa::input]
        fn analyzer_config(&self) -> AnalyzerConfig;

        #[salsa::input]
        fn use_prelude(&self) -> bool;

        fn ast_package_inner(&self, path: String) -> NeverEq<Arc<ast::Package>>;

        #[salsa::transparent]
        fn ast_package(&self, path: String) -> Option<Arc<ast::Package>>;

        fn internal_prelude(&self) -> NeverEq<Result<Arc<PackageExports>, Arc<FileErrors>>>;

        fn prelude_inner(&self) -> NeverEq<Result<Arc<PackageExports>, Arc<FileErrors>>>;

        #[salsa::transparent]
        fn prelude(&self) -> Result<Arc<PackageExports>, Arc<FileErrors>>;

        #[salsa::cycle(recover_cycle2)]
        #[allow(clippy::type_complexity)]
        fn semantic_package_inner(
            &self,
            path: String,
        ) -> NeverEq<SalvageResult<(Arc<PackageExports>, Arc<nodes::Package>), Arc<FileErrors>>>;

        #[salsa::transparent]
        fn semantic_package(
            &self,
            path: String,
        ) -> SalvageResult<(Arc<PackageExports>, Arc<nodes::Package>), Arc<FileErrors>>;

        #[salsa::cycle(recover_cycle)]
        fn semantic_package_cycle(
            &self,
            path: String,
        ) -> NeverEq<Result<Arc<PackageExports>, nodes::ErrorKind>>;
    }

    /// Storage for flux programs and their intermediates
    #[salsa::database(FluxStorage)]
    pub struct Database {
        storage: salsa::Storage<Self>,
        pub(crate) packages: Mutex<HashSet<String>>,
    }

    impl Default for Database {
        fn default() -> Self {
            let mut db = Self {
                storage: Default::default(),
                packages: Default::default(),
            };
            db.set_analyzer_config(AnalyzerConfig::default());
            db.set_use_prelude(true);
            db
        }
    }

    impl salsa::Database for Database {}

    impl FluxBase for Database {
        fn has_package(&self, package: &str) -> bool {
            self.packages.lock().unwrap().contains(package)
        }

        fn package_files(&self, package: &str) -> Vec<String> {
            let packages = self.packages.lock().unwrap();
            let found_packages = packages
                .iter()
                .filter(|p| {
                    p.starts_with(package)
                        && p[package.len()..].starts_with('/')
                        && p[package.len() + 1..].split('/').count() == 1
                })
                .cloned()
                .collect::<Vec<_>>();

            assert!(
                !packages.is_empty(),
                "Did not find any package files for `{}`",
                package,
            );

            found_packages
        }

        fn source(&self, path: String) -> Arc<str> {
            self.source_inner(path)
        }

        fn set_source(&mut self, path: String, source: Arc<str>) {
            self.packages.lock().unwrap().insert(path.clone());

            self.set_source_inner(path, source)
        }
    }

    fn ast_package_inner_2(db: &dyn Flux, path: String) -> Arc<ast::Package> {
        let files = db
            .package_files(&path)
            .into_iter()
            .map(|file_path| {
                let source = db.source(file_path.clone());

                parser::parse_string(file_path, &source)
            })
            .collect::<Vec<_>>();

        Arc::new(ast::Package {
            base: ast::BaseNode::default(),
            path,
            package: String::from(files[0].get_package()),
            files,
        })
    }

    fn ast_package_inner(db: &dyn Flux, path: String) -> NeverEq<Arc<ast::Package>> {
        NeverEq(ast_package_inner_2(db, path))
    }

    fn ast_package(db: &dyn Flux, path: String) -> Option<Arc<ast::Package>> {
        if db.has_package(&path) {
            Some(db.ast_package_inner(path).0)
        } else {
            None
        }
    }

    fn internal_prelude_inner(db: &dyn Flux) -> Result<Arc<PackageExports>, Arc<FileErrors>> {
        let mut prelude_map = PackageExports::new();
        for name in INTERNAL_PRELUDE {
            // Infer each package in the prelude allowing the earlier packages to be used by later
            // packages within the prelude list.
            let (types, _sem_pkg) = db.semantic_package(name.into()).map_err(|err| err.error)?;

            prelude_map.copy_bindings_from(&types);
        }
        Ok(Arc::new(prelude_map))
    }

    fn internal_prelude(db: &dyn Flux) -> NeverEq<Result<Arc<PackageExports>, Arc<FileErrors>>> {
        NeverEq(internal_prelude_inner(db))
    }

    fn prelude_inner_2(db: &dyn Flux) -> Result<Arc<PackageExports>, Arc<FileErrors>> {
        let mut prelude_map = PackageExports::new();
        for name in PRELUDE {
            // Infer each package in the prelude allowing the earlier packages to be used by later
            // packages within the prelude list.
            let (types, _sem_pkg) = db.semantic_package(name.into()).map_err(|err| err.error)?;

            prelude_map.copy_bindings_from(&types);
        }
        Ok(Arc::new(prelude_map))
    }

    fn prelude_inner(db: &dyn Flux) -> NeverEq<Result<Arc<PackageExports>, Arc<FileErrors>>> {
        NeverEq(prelude_inner_2(db))
    }

    fn prelude(db: &dyn Flux) -> Result<Arc<PackageExports>, Arc<FileErrors>> {
        db.prelude_inner().0
    }

    fn semantic_package_inner_2(
        db: &dyn Flux,
        path: String,
    ) -> SalvageResult<(Arc<PackageExports>, Arc<nodes::Package>), Arc<FileErrors>> {
        let prelude = if !db.use_prelude() || INTERNAL_PRELUDE.contains(&&path[..]) {
            Default::default()
        } else if [
            "system",
            "date",
            "math",
            "strings",
            "regexp",
            "experimental/table",
        ]
        .contains(&&path[..])
            || PRELUDE.contains(&&path[..])
        {
            db.internal_prelude().0?
        } else {
            db.prelude()?
        };

        semantic_package_with_prelude(db, path, &prelude)
    }

    #[allow(clippy::type_complexity)]
    fn semantic_package_inner(
        db: &dyn Flux,
        path: String,
    ) -> NeverEq<SalvageResult<(Arc<PackageExports>, Arc<nodes::Package>), Arc<FileErrors>>> {
        NeverEq(semantic_package_inner_2(db, path))
    }

    fn semantic_package(
        db: &dyn Flux,
        path: String,
    ) -> SalvageResult<(Arc<PackageExports>, Arc<nodes::Package>), Arc<FileErrors>> {
        db.semantic_package_inner(path).0
    }

    fn semantic_package_with_prelude(
        db: &dyn Flux,
        path: String,
        prelude: &PackageExports,
    ) -> SalvageResult<(Arc<PackageExports>, Arc<nodes::Package>), Arc<FileErrors>> {
        let file = db.ast_package_inner(path).0;

        let env = Environment::new(prelude.into());
        let mut importer = &*db;
        let mut analyzer = Analyzer::new(env, &mut importer, db.analyzer_config());
        let (exports, sem_pkg) = analyzer.analyze_ast(&file).map_err(|err| {
            err.map(|(exports, sem_pkg)| (Arc::new(exports), Arc::new(sem_pkg)))
                .map_err(Arc::new)
        })?;

        Ok((Arc::new(exports), Arc::new(sem_pkg)))
    }

    fn semantic_package_cycle(
        db: &dyn Flux,
        path: String,
    ) -> NeverEq<Result<Arc<PackageExports>, nodes::ErrorKind>> {
        NeverEq(
            db.semantic_package(path.clone())
                .ok()
                .map(|(exports, _)| exports)
                .ok_or_else(|| nodes::ErrorKind::InvalidImportPath(path.clone())),
        )
    }

    fn recover_cycle2<T>(
        _db: &dyn Flux,
        cycle: &[String],
        name: &str,
    ) -> NeverEq<SalvageResult<T, Arc<FileErrors>>> {
        let mut cycle: Vec<_> = cycle
            .iter()
            .filter(|k| k.starts_with("semantic_package_cycle("))
            .map(|k| {
                k.trim_matches(|c: char| c != '"')
                    .trim_matches('"')
                    .trim_start_matches('@')
                    .to_string()
            })
            .collect();
        cycle.pop();

        NeverEq(Err(Arc::new(FileErrors {
            file: name.to_owned(),
            source: None,
            diagnostics: From::from(located(
                Default::default(),
                semantic::ErrorKind::Inference(nodes::ErrorKind::ImportCycle { cycle }),
            )),
        })
        .into()))
    }
    fn recover_cycle<T>(
        _db: &dyn Flux,
        cycle: &[String],
        _name: &str,
    ) -> NeverEq<Result<T, nodes::ErrorKind>> {
        // We get a list of strings like "semantic_package_inner(\"b\")",
        let mut cycle: Vec<_> = cycle
            .iter()
            .filter(|k| k.starts_with("semantic_package_inner("))
            .map(|k| {
                k.trim_matches(|c: char| c != '"')
                    .trim_matches('"')
                    .to_string()
            })
            .collect();
        cycle.pop();

        NeverEq(Err(nodes::ErrorKind::ImportCycle { cycle }))
    }

    impl Importer for Database {
        fn import(&mut self, path: &str) -> Result<PolyType, nodes::ErrorKind> {
            self.semantic_package_cycle(path.into())
                .0
                .map(|exports| exports.typ())
        }
        fn symbol(&mut self, path: &str, symbol_name: &str) -> Option<Symbol> {
            self.semantic_package_cycle(path.into())
                .0
                .ok()
                .and_then(|exports| exports.lookup_symbol(symbol_name).cloned())
        }
    }

    impl Importer for &dyn Flux {
        fn import(&mut self, path: &str) -> Result<PolyType, nodes::ErrorKind> {
            self.semantic_package_cycle(path.into())
                .0
                .map(|exports| exports.typ())
        }
        fn symbol(&mut self, path: &str, symbol_name: &str) -> Option<Symbol> {
            self.semantic_package_cycle(path.into())
                .0
                .ok()
                .and_then(|exports| exports.lookup_symbol(symbol_name).cloned())
        }
    }
}
pub use self::db::{Database, Flux, FluxBase};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, parser, semantic::convert::convert_polytype};

    #[test]
    fn infer_program() -> Result<()> {
        let a = r#"
            f = (x) => x
        "#;
        let b = r#"
            import "a"

            builtin x : int

            y = a.f(x: x)
        "#;
        let c = r#"
            package c
            import "b"

            z = b.y
        "#;

        let mut db = Database::default();
        db.set_use_prelude(false);

        for (k, v) in [("a/a.flux", a), ("b/b.flux", b), ("c/c.flux", c)] {
            db.set_source(k.into(), v.into());
        }
        let (types, _) = db.semantic_package("c".into())?;

        let want = PackageExports::try_from(vec![(types.lookup_symbol("z").unwrap().clone(), {
            let mut p = parser::Parser::new("int");
            let typ_expr = p.parse_type_expression();
            if let Err(err) = ast::check::check(ast::walk::Node::TypeExpression(&typ_expr)) {
                panic!("TypeExpression parsing failed for int. {:?}", err);
            }
            convert_polytype(&typ_expr, &Default::default())?
        })])
        .unwrap();
        if want != *types {
            bail!(
                "unexpected inference result:\n\nwant: {:?}\n\ngot: {:?}",
                want,
                types,
            );
        }

        let a = {
            let mut p = parser::Parser::new("{f: (x: A) => A}");
            let typ_expr = p.parse_type_expression();
            if let Err(err) = ast::check::check(ast::walk::Node::TypeExpression(&typ_expr)) {
                panic!("TypeExpression parsing failed for int. {:?}", err);
            }
            convert_polytype(&typ_expr, &Default::default())?
        };
        assert_eq!(db.import("a"), Ok(a));

        let b = {
            let mut p = parser::Parser::new("{x: int , y: int}");
            let typ_expr = p.parse_type_expression();
            if let Err(err) = ast::check::check(ast::walk::Node::TypeExpression(&typ_expr)) {
                panic!("TypeExpression parsing failed for int. {:?}", err);
            }
            convert_polytype(&typ_expr, &Default::default())?
        };
        assert_eq!(db.import("b"), Ok(b));

        Ok(())
    }

    #[test]
    fn cyclic_dependency() {
        let a = r#"
            import "b"
        "#;
        let b = r#"
            import "a"
        "#;

        let mut db = Database::default();

        db.set_use_prelude(false);

        for (k, v) in [("a/a.flux", a), ("b/b.flux", b)] {
            db.set_source(k.into(), v.into());
        }

        let got_err = db
            .semantic_package("b".into())
            .expect_err("expected cyclic dependency error");

        assert_eq!(
            r#"package "b" depends on itself"#.to_string(),
            got_err.to_string(),
        );
    }

    #[test]
    fn bootstrap() {
        infer_stdlib_dir("../../stdlib", AnalyzerConfig::default())
            .unwrap_or_else(|err| panic!("{}", err));
    }
}
