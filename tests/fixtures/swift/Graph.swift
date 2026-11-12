func swiftGraphLeaf(_ value: Int) -> Int { value }

func swiftGraphDirect() -> Int { swiftGraphLeaf(1) }
func swiftGraphReference() -> (Int) -> Int { swiftGraphLeaf }

struct SwiftGraphBox {
    var stored = swiftGraphLeaf(2)
    var computed: Int { swiftGraphLeaf(stored) }
    func method() -> Int { computed }
}

let swiftGraphLoaded = swiftGraphLeaf(3)
let swiftGraphClosure: () -> Int = { swiftGraphLeaf(4) }
