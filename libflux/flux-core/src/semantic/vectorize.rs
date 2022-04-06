use std::collections::HashMap;

use crate::{
    errors::located,
    semantic::{
        nodes::{
            Block, ErrorKind, Expression, FunctionExpr, IdentifierExpr, MemberExpr, ObjectExpr,
            Package, Property, Result, ReturnStmt,
        },
        types::{self, Function, Label, MonoType},
        Symbol,
    },
};

/// Vectorizes a pkg
pub fn vectorize(pkg: &mut Package) -> Result<()> {
    use crate::semantic::walk::{walk_mut, NodeMut, VisitorMut};
    struct Vectorizer {
        result: Result<()>,
    }
    impl VisitorMut for Vectorizer {
        fn visit(&mut self, _node: &mut NodeMut) -> bool {
            self.result.is_ok()
        }

        fn done(&mut self, node: &mut NodeMut) {
            if let NodeMut::FunctionExpr(function) = node {
                match function.vectorize() {
                    Ok(vectorized) => function.vectorized = Some(Box::new(vectorized)),
                    Err(err) => self.result = Err(err),
                }
            }
        }
    }

    let mut visitor = Vectorizer { result: Ok(()) };
    walk_mut(&mut visitor, NodeMut::Package(pkg));
    visitor.result
}

type VectorizeEnv = HashMap<Symbol, MonoType>;

impl Expression {
    fn vectorize(&self, env: &VectorizeEnv) -> Result<Self> {
        Ok(match self {
            Expression::Identifier(identifier) => {
                Expression::Identifier(identifier.vectorize(env)?)
            }
            Expression::Member(member) => {
                let object = member.object.vectorize(env)?;
                let typ = object.type_of();
                Expression::Member(Box::new(MemberExpr {
                    loc: member.loc.clone(),
                    typ: typ
                        .field(&member.property)
                        .ok_or_else(|| {
                            located(
                                member.object.loc().clone(),
                                ErrorKind::UnableToVectorize(format!(
                                    "Expected record type, got `{}`",
                                    typ
                                )),
                            )
                        })?
                        .v
                        .clone(),
                    object,
                    property: member.property.clone(),
                }))
            }
            _ => {
                return Err(located(
                    self.loc().clone(),
                    ErrorKind::UnableToVectorize("Unable to vectorize expression".into()),
                ))
            }
        })
    }
}

impl IdentifierExpr {
    fn vectorize(&self, env: &VectorizeEnv) -> Result<Self> {
        let typ = env.get(&self.name).unwrap_or(&self.typ).clone();

        Ok(IdentifierExpr {
            loc: self.loc.clone(),
            typ,
            name: self.name.clone(),
        })
    }
}

impl FunctionExpr {
    fn vectorize(&self) -> Result<Self> {
        if self.params.len() == 1 && self.params[0].key.name == "r" {
            fn vectorize_fields(record: &MonoType) -> MonoType {
                use crate::semantic::types::Record;
                match record {
                    MonoType::Record(record) => MonoType::from(match &**record {
                        Record::Empty => Record::Empty,
                        Record::Extension { head, tail } => Record::Extension {
                            head: types::Property {
                                k: head.k.clone(),
                                v: MonoType::vector(head.v.clone()),
                            },
                            tail: vectorize_fields(tail),
                        },
                    }),
                    _ => record.clone(),
                }
            }
            let params: Vec<_> = self
                .params
                .iter()
                .map(|param| {
                    let parameter_type =
                        vectorize_fields(self.typ.parameter(&param.key.name).unwrap());
                    (param.key.name.clone(), parameter_type)
                })
                .collect();
            let env: VectorizeEnv = params.iter().cloned().collect();

            let body = match &self.body {
                Block::Variable(..) | Block::Expr(..) => {
                    return Err(located(
                        self.body.loc().clone(),
                        ErrorKind::UnableToVectorize("Unable to vectorize statements".into()),
                    ))
                }
                // XXX: sean (January 14 2022) - The only type of function expression
                // currently supported for vectorization is one whose body contains only
                // a single object expression, the fields of which only reference members of
                // `r` and do not include any kind of operation, literal, or logical expression.
                //
                // We may support other expression types in the future.
                Block::Return(e) => {
                    let argument = match &e.argument {
                        Expression::Object(e) => {
                            let properties = e
                                .properties
                                .iter()
                                .map(|p| {
                                    let mem = match &p.value {
                                        Expression::Member(m) => m.clone(),
                                        _ => {
                                            return Err(located(
                                                self.body.loc().clone(),
                                                ErrorKind::UnableToVectorize(
                                                    "expression type cannot be vectorized".into(),
                                                ),
                                            ))
                                        }
                                    };
                                    match mem.object {
                                        Expression::Identifier(i) if i.name == "r" => {
                                            Ok(Property {
                                                loc: p.loc.clone(),
                                                key: p.key.clone(),
                                                value: p.value.vectorize(&env)?,
                                            })
                                        }
                                        _ => {
                                            return Err(located(
                                                self.body.loc().clone(),
                                                ErrorKind::UnableToVectorize(
                                                    "expression type cannot be vectorized".into(),
                                                ),
                                            ))
                                        }
                                    }
                                })
                                .collect::<Result<Vec<_>>>()?;

                            let with = e
                                .with
                                .as_ref()
                                .map(|with| with.vectorize(&env))
                                .transpose()?;

                            Expression::Object(Box::new(ObjectExpr {
                                loc: e.loc.clone(),
                                typ: MonoType::from(types::Record::new(
                                    properties.iter().map(|p| types::Property {
                                        k: Label::from(p.key.name.clone()),
                                        v: p.value.type_of(),
                                    }),
                                    with.as_ref().map(|with| with.typ.clone()),
                                )),
                                with,
                                properties,
                            }))
                        }
                        _ => {
                            return Err(located(
                                e.argument.loc().clone(),
                                ErrorKind::UnableToVectorize(
                                    "Vectorization only supports returning a record".into(),
                                ),
                            ))
                        }
                    };
                    Block::Return(ReturnStmt {
                        loc: e.loc.clone(),
                        argument,
                    })
                }
            };
            Ok(FunctionExpr {
                loc: self.loc.clone(),
                typ: MonoType::from(Function {
                    pipe: None,
                    req: params
                        .into_iter()
                        .map(|(key, value)| (key.to_string(), value))
                        .collect(),
                    opt: Default::default(),
                    retn: body.type_of(),
                }),
                params: self.params.clone(),
                body,
                vectorized: None,
            })
        } else {
            // Only `map` will get vectorized to start with, so only try to vectorize such functions
            Err(located(
                self.loc.clone(),
                ErrorKind::UnableToVectorize("Does not match the `map` signature".into()),
            ))
        }
    }
}