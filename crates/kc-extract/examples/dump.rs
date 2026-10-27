//! Print the tree-sitter tree for a Swift, Rust or Python file: `cargo run -p kc-extract --example dump -- file.swift`.
fn main() {
    let path = std::env::args().nth(1).expect("usage: dump <file.swift|.rs|.py>");
    let src = std::fs::read_to_string(&path).unwrap();
    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = match std::path::Path::new(&path).extension().and_then(|e| e.to_str()) {
        Some("rs") => tree_sitter_rust::LANGUAGE.into(),
        Some("py") => tree_sitter_python::LANGUAGE.into(),
        _ => tree_sitter_swift::LANGUAGE.into(),
    };
    parser.set_language(&lang).unwrap();
    let tree = parser.parse(&src, None).unwrap();
    print_node(tree.root_node(), &src, 0, None);
}

fn print_node(node: tree_sitter::Node, src: &str, depth: usize, field: Option<&str>) {
    let text = if node.child_count() == 0 { format!(" {:?}", &src[node.byte_range()]) } else { String::new() };
    let field = field.map(|f| format!("{f}: ")).unwrap_or_default();
    println!("{}{}{}{} [{}]", "  ".repeat(depth), field, node.kind(), text, node.start_position().row + 1);
    let mut cursor = node.walk();
    for (i, child) in node.children(&mut cursor).enumerate() {
        print_node(child, src, depth + 1, node.field_name_for_child(i as u32));
    }
}
