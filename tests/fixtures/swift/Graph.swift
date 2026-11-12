func swiftGraphLeaf(_ value: Int) -> Int { value }

func swiftGraphDirect() -> Int { swiftGraphLeaf(1) }
func swiftGraphReference() -> (Int) -> Int { swiftGraphLeaf }

protocol SwiftGraphContract { func method() -> Int }

struct SwiftGraphBox: SwiftGraphContract {
    var stored = swiftGraphLeaf(2)
    var computed: Int { swiftGraphLeaf(stored) }
    func method() -> Int { computed }
}

let swiftGraphLoaded = swiftGraphLeaf(3)
let swiftGraphClosure: () -> Int = { swiftGraphLeaf(4) }

class SwiftGraphBase {
    func work() -> Int { swiftGraphLeaf(5) }
}
class SwiftGraphDerived: SwiftGraphBase {
    override func work() -> Int { super.work() }
}
