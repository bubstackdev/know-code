use std::collections::HashMap;

pub struct Ledger {
    entries: HashMap<String, i64>,
}

impl Ledger {
    pub fn new() -> Self {
        Ledger { entries: HashMap::new() }
    }

    pub fn add(&mut self, key: &str, amount: i64) {
        let e = self.entries.entry(key.to_string()).or_insert(0);
        *e += amount;
    }

    pub fn get(&self, key: &str) -> i64 {
        *self.entries.get(key).unwrap()
    }

    pub fn parse(text: &str) -> i64 {
        let n: i64 = text.trim().parse().expect("a number");
        println!("parsed {}", n);
        n
    }

    pub fn risky(&self) -> Option<i64> {
        let v = self.entries.values().next().unwrap();
        if *v < 0 {
            panic!("negative");
        }
        Some(*v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        let mut l = Ledger::new();
        l.add("a", 1);
        assert_eq!(l.get("a"), 1);
        let x: Option<i64> = None;
        x.unwrap_or(0);
    }
}
