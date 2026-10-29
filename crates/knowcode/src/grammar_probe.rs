//! Throwaway grammar size probe (W1, eval/size/grammars.md); never merged. Each `g-<name>` feature links one grammar,
//! and `KC_GRAMMAR_PROBE=1 knowcode` reads every language the grammar's crate exports, so LTO keeps all of it.

#[allow(unused_mut)]
pub fn languages() -> Vec<(&'static str, tree_sitter::Language)> {
    let mut out: Vec<(&'static str, tree_sitter::Language)> = Vec::new();
    #[cfg(feature = "g-go")]
    out.push(("go:LANGUAGE", probe_go::LANGUAGE.into()));
    #[cfg(feature = "g-java")]
    out.push(("java:LANGUAGE", probe_java::LANGUAGE.into()));
    #[cfg(feature = "g-kotlin")]
    out.push(("kotlin:LANGUAGE", probe_kotlin::LANGUAGE.into()));
    #[cfg(feature = "g-php")]
    out.push(("php:LANGUAGE_PHP", probe_php::LANGUAGE_PHP.into()));
    #[cfg(feature = "g-php")]
    out.push(("php:LANGUAGE_PHP_ONLY", probe_php::LANGUAGE_PHP_ONLY.into()));
    #[cfg(feature = "g-ruby")]
    out.push(("ruby:LANGUAGE", probe_ruby::LANGUAGE.into()));
    #[cfg(feature = "g-c")]
    out.push(("c:LANGUAGE", probe_c::LANGUAGE.into()));
    #[cfg(feature = "g-cpp")]
    out.push(("cpp:LANGUAGE", probe_cpp::LANGUAGE.into()));
    #[cfg(feature = "g-objc")]
    out.push(("objc:LANGUAGE", probe_objc::LANGUAGE.into()));
    #[cfg(feature = "g-vue")]
    out.push(("vue:LANGUAGE", probe_vue::LANGUAGE.into()));
    #[cfg(feature = "g-svelte")]
    out.push(("svelte:LANGUAGE", probe_svelte::LANGUAGE.into()));
    #[cfg(feature = "g-scala")]
    out.push(("scala:LANGUAGE", probe_scala::LANGUAGE.into()));
    #[cfg(feature = "g-dart")]
    out.push(("dart:LANGUAGE", probe_dart::LANGUAGE.into()));
    #[cfg(feature = "g-lua")]
    out.push(("lua:LANGUAGE", probe_lua::LANGUAGE.into()));
    #[cfg(feature = "g-elixir")]
    out.push(("elixir:LANGUAGE", probe_elixir::LANGUAGE.into()));
    #[cfg(feature = "g-erlang")]
    out.push(("erlang:LANGUAGE", probe_erlang::LANGUAGE.into()));
    #[cfg(feature = "g-haskell")]
    out.push(("haskell:LANGUAGE", probe_haskell::LANGUAGE.into()));
    #[cfg(feature = "g-ocaml")]
    out.push(("ocaml:LANGUAGE_OCAML", probe_ocaml::LANGUAGE_OCAML.into()));
    #[cfg(feature = "g-ocaml")]
    out.push(("ocaml:LANGUAGE_OCAML_INTERFACE", probe_ocaml::LANGUAGE_OCAML_INTERFACE.into()));
    #[cfg(feature = "g-ocaml")]
    out.push(("ocaml:LANGUAGE_OCAML_TYPE", probe_ocaml::LANGUAGE_OCAML_TYPE.into()));
    #[cfg(feature = "g-zig")]
    out.push(("zig:LANGUAGE", probe_zig::LANGUAGE.into()));
    #[cfg(feature = "g-bash")]
    out.push(("bash:LANGUAGE", probe_bash::LANGUAGE.into()));
    #[cfg(feature = "g-r")]
    out.push(("r:LANGUAGE", probe_r::LANGUAGE.into()));
    #[cfg(feature = "g-julia")]
    out.push(("julia:LANGUAGE", probe_julia::LANGUAGE.into()));
    #[cfg(feature = "g-perl")]
    out.push(("perl:LANGUAGE", probe_perl::LANGUAGE.into()));
    #[cfg(feature = "g-clojure")]
    out.push(("clojure:LANGUAGE", probe_clojure::LANGUAGE.into()));
    #[cfg(feature = "g-elm")]
    out.push(("elm:LANGUAGE", probe_elm::LANGUAGE.into()));
    #[cfg(feature = "g-nix")]
    out.push(("nix:LANGUAGE", probe_nix::LANGUAGE.into()));
    #[cfg(feature = "g-groovy")]
    out.push(("groovy:LANGUAGE", probe_groovy::LANGUAGE.into()));
    #[cfg(feature = "g-powershell")]
    out.push(("powershell:LANGUAGE", probe_powershell::LANGUAGE.into()));
    #[cfg(feature = "g-fortran")]
    out.push(("fortran:LANGUAGE", probe_fortran::LANGUAGE.into()));
    #[cfg(feature = "g-solidity")]
    out.push(("solidity:LANGUAGE", probe_solidity::LANGUAGE.into()));
    #[cfg(feature = "g-fsharp")]
    out.push(("fsharp:LANGUAGE_FSHARP", probe_fsharp::LANGUAGE_FSHARP.into()));
    #[cfg(feature = "g-fsharp")]
    out.push(("fsharp:LANGUAGE_SIGNATURE", probe_fsharp::LANGUAGE_SIGNATURE.into()));
    #[cfg(feature = "g-gleam")]
    out.push(("gleam:LANGUAGE", probe_gleam::LANGUAGE.into()));
    #[cfg(feature = "g-verilog")]
    out.push(("verilog:LANGUAGE", probe_verilog::LANGUAGE.into()));
    #[cfg(feature = "g-cuda")]
    out.push(("cuda:LANGUAGE", probe_cuda::LANGUAGE.into()));
    #[cfg(feature = "g-matlab")]
    out.push(("matlab:LANGUAGE", probe_matlab::LANGUAGE.into()));
    #[cfg(feature = "g-nim")]
    out.push(("nim:LANGUAGE", probe_nim::LANGUAGE.into()));
    out
}

pub fn print() {
    for (name, lang) in languages() {
        println!("{name} abi {} kinds {} fields {}", lang.abi_version(), lang.node_kind_count(), lang.field_count());
    }
}
