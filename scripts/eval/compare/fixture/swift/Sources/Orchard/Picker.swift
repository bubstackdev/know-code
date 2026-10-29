public protocol Picker {
    func pick(_ basket: Basket)
}

public struct HandPicker: Picker {
    public init() {}

    public func pick(_ basket: Basket) {
        basket.items.append("pear")
    }
}
