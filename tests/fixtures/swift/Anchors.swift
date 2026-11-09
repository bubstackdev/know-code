struct AnchorBox {
    var values: [Int]
    var size: Int { values.count }
    subscript(index: Int) -> Int { values[index] }
}
postfix operator +
postfix func + (value: Int) -> Int { value }
func consumeAnchors() -> Int {
    let box = AnchorBox(values: [1, 2, 3])
    let copied = [Int](repeating: box[0], count: 2)
    let bounded = 0+..<box.size
    let closed = 0...box.size
    return copied[0] + bounded.count + closed.count
}
