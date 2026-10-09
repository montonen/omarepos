//! Language detection by file extension.
//!
//! Deliberately a lookup table rather than a dependency: we only ever classify
//! paths already listed in the git index, so breadth matters more than nuance.
//!
//! Each language had a colour as well, for the bar of them a card used to
//! draw. The bar is gone; the names are what `lang:` in the filter matches
//! and what a card's tooltip lists.

/// Map a file name to a language's name. Returns `None` for anything we
/// don't count (lock files, assets, data) so those don't skew the mix.
pub fn classify(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);

    // A few whole-file names carry more signal than their extension.
    match file {
        "Dockerfile" | "Containerfile" => return Some("Docker"),
        "Makefile" | "GNUmakefile" => return Some("Make"),
        "justfile" | "Justfile" => return Some("Just"),
        "Gemfile" | "Rakefile" => return Some("Ruby"),
        _ => {}
    }

    let ext = file.rsplit_once('.').map(|(_, e)| e)?;
    let l = match ext.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "go" => "Go",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "rb" | "erb" | "rake" => "Ruby",
        "py" | "pyi" => "Python",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TSX",
        "js" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JSX",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "php" => "PHP",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "hs" => "Haskell",
        "ml" | "mli" => "OCaml",
        "scala" | "sbt" => "Scala",
        "clj" | "cljs" | "cljc" => "Clojure",
        "lua" => "Lua",
        "zig" => "Zig",
        "dart" => "Dart",
        "nim" => "Nim",
        "jl" => "Julia",
        "r" => "R",
        "sh" | "bash" | "zsh" | "fish" => "Shell",
        "nu" => "Nushell",
        "ps1" => "PowerShell",
        "sql" => "SQL",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "Sass",
        "qml" => "QML",
        "glsl" | "vert" | "frag" | "wgsl" => "Shader",
        "gd" => "GDScript",
        "nix" => "Nix",
        "tf" | "hcl" => "HCL",
        "vim" => "Vim Script",
        "el" | "lisp" | "scm" => "Lisp",
        "md" | "markdown" => "Markdown",
        "tex" => "TeX",
        "yml" | "yaml" => "YAML",
        "toml" => "TOML",
        "json" | "jsonc" => "JSON",
        "xml" | "xsl" => "XML",
        "proto" => "Protobuf",
        _ => return None,
    };
    Some(l)
}
