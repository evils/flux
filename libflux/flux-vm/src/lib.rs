// use fluxcore::semantic;
//
// pub struct Machine<T: Runtime> {
//     runtime: T
// }
//
// impl <T: Runtime> Machine<T> {
//     pub fn new(runtime: impl Runtime) -> Box<Machine<T>> {
//         return Box::new(Machine { runtime });
//     }
//
//     pub fn run(&self) {
//
//     }
// }



pub enum Value {
    Int(i64),
    Float(f64),
}

pub trait Runtime {
    fn print(&self, value: &Value) {
        match value {
            Value::Int(n) => println!("{}", n),
            Value::Float(n) => println!("{}", n),
        }
    }
}
