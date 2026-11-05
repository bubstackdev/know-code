public final class Basket {
    public var items: [String] = []

    public init() {}

    public func weigh() -> Int {
        return items.count
    }
}
