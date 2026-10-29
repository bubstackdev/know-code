mod ledger;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let first = args.get(1).unwrap();
    let total = ledger::Ledger::parse(first);
    println!("{}", total);
    dbg!(total);
    todo!();
}
