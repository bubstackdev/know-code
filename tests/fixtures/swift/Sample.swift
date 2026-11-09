struct Café {
    func greet(_ name: String) -> String {
        let message = name
        return message
    }
}
func use() -> String { Café().greet("a") }
