public func gather(_ picker: Picker) -> Int {
    let basket = Basket()
    picker.pick(basket)
    return basket.weigh()
}

public func harvest() -> Int {
    return gather(HandPicker()) + gather(HandPicker())
}

public func report() -> String {
    return "harvested \(harvest())"
}
