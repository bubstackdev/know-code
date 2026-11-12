pub struct Greeter;
mod helper;

#[cfg(know_code_generated)]
pub fn only_after_build_script() {}

impl Greeter {
    pub fn greet(&self, input: &str) -> String {
        let café = format!("Hello {input}"); // 🦀 UTF-8 source coordinates
        café
    }
}

pub fn use_greeter() -> String {
    let greeter = Greeter;
    helper::decorate(greeter.greet("Know Code"))
}

pub fn shadowed() -> usize {
    let café = 42;
    café
}

#[cfg(test)]
mod tests {
    use super::Greeter;

    #[test]
    fn greeting() {
        let expected = "Hello Know Code";
        assert_eq!(Greeter.greet("Know Code"), expected);
    }
}

const GRAPH_VALUE: i32 = 7;

pub fn graph_leaf(value: i32) -> i32 {
    value + GRAPH_VALUE
}

pub fn graph_entry() -> i32 {
    graph_leaf(GRAPH_VALUE)
}

// Taking a function value is an impact dependency, not an invocation.
pub fn graph_value_only() -> fn(i32) -> i32 {
    graph_leaf
}

pub fn graph_nested() -> i32 {
    fn graph_inner() -> i32 { graph_leaf(1) }
    graph_inner()
}

// Deferred execution must not be attributed to this containing function.
pub fn graph_deferred() {
    let _closure = || graph_leaf(2);
    let _future = async { graph_leaf(3) };
}

pub fn graph_type_reference(value: &Greeter) -> &Greeter {
    value
}
