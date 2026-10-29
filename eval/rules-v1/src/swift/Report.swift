import Foundation

struct Report {
    let rows: [String]

    func render() -> String {
        print("rendering")
        var out = ""
        for row in rows {
            out += row
        }
        let first = rows.first as! String
        return out + first
    }
}
