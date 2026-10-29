// A made-up account, for the rules fixture.
import Foundation

final class Account {
    var balance: Decimal = 0
    weak var delegate: AccountDelegate?

    func deposit(_ amount: Decimal) {
        print("deposit \(amount)")
        balance += amount
    }

    func load(from url: URL) -> Data {
        let data = try! Data(contentsOf: url)
        return data
    }

    func name(for id: Int) -> String {
        let names = ["a", "b"]
        return names.first!
    }

    func save() {
        DispatchQueue.main.async {
            self.delegate?.didSave(self)
        }
        do {
            try FileManager.default.removeItem(atPath: "/tmp/fixture")
        } catch {
        }
    }
}

protocol AccountDelegate: AnyObject {
    func didSave(_ account: Account)
}
