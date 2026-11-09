func choose(_ value: Int) -> Int { value }
func choose(_ value: String) -> String { value }
func consume() -> Int {
    let café = choose(1)
    let text = choose("ok")
    _ = text
    return café
}
