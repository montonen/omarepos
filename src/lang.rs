//! Language detection by file extension, with GitHub-ish colours.
//!
//! Deliberately a lookup table rather than a dependency: we only ever classify
//! paths already listed in the git index, so breadth matters more than nuance.

pub struct Lang {
    pub name: &'static str,
    pub color: &'static str,
}

const fn lang(name: &'static str, color: &'static str) -> Lang {
    Lang { name, color }
}

/// Map a file name to a language. Returns `None` for anything we don't count
/// (lock files, assets, data) so those don't skew the mix.
pub fn classify(path: &str) -> Option<Lang> {
    let file = path.rsplit('/').next().unwrap_or(path);

    // A few whole-file names carry more signal than their extension.
    match file {
        "Dockerfile" | "Containerfile" => return Some(lang("Docker", "#384d54")),
        "Makefile" | "GNUmakefile" => return Some(lang("Make", "#427819")),
        "justfile" | "Justfile" => return Some(lang("Just", "#384d54")),
        "Gemfile" | "Rakefile" => return Some(lang("Ruby", "#701516")),
        _ => {}
    }

    let ext = file.rsplit_once('.').map(|(_, e)| e)?;
    let l = match ext.to_ascii_lowercase().as_str() {
        "rs" => lang("Rust", "#dea584"),
        "go" => lang("Go", "#00add8"),
        "c" | "h" => lang("C", "#555555"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => lang("C++", "#f34b7d"),
        "cs" => lang("C#", "#178600"),
        "java" => lang("Java", "#b07219"),
        "kt" | "kts" => lang("Kotlin", "#a97bff"),
        "swift" => lang("Swift", "#f05138"),
        "rb" | "erb" | "rake" => lang("Ruby", "#701516"),
        "py" | "pyi" => lang("Python", "#3572a5"),
        "ts" | "mts" | "cts" => lang("TypeScript", "#3178c6"),
        "tsx" => lang("TSX", "#3178c6"),
        "js" | "mjs" | "cjs" => lang("JavaScript", "#f1e05a"),
        "jsx" => lang("JSX", "#f1e05a"),
        "vue" => lang("Vue", "#41b883"),
        "svelte" => lang("Svelte", "#ff3e00"),
        "php" => lang("PHP", "#4f5d95"),
        "ex" | "exs" => lang("Elixir", "#6e4a7e"),
        "erl" | "hrl" => lang("Erlang", "#b83998"),
        "hs" => lang("Haskell", "#5e5086"),
        "ml" | "mli" => lang("OCaml", "#ef7a08"),
        "scala" | "sbt" => lang("Scala", "#c22d40"),
        "clj" | "cljs" | "cljc" => lang("Clojure", "#db5855"),
        "lua" => lang("Lua", "#000080"),
        "zig" => lang("Zig", "#ec915c"),
        "dart" => lang("Dart", "#00b4ab"),
        "nim" => lang("Nim", "#ffc200"),
        "jl" => lang("Julia", "#a270ba"),
        "r" => lang("R", "#198ce7"),
        "sh" | "bash" | "zsh" | "fish" => lang("Shell", "#89e051"),
        "nu" => lang("Nushell", "#3aa675"),
        "ps1" => lang("PowerShell", "#012456"),
        "sql" => lang("SQL", "#e38c00"),
        "html" | "htm" => lang("HTML", "#e34c26"),
        "css" => lang("CSS", "#563d7c"),
        "scss" | "sass" => lang("Sass", "#c6538c"),
        "qml" => lang("QML", "#44a51c"),
        "glsl" | "vert" | "frag" | "wgsl" => lang("Shader", "#5686a5"),
        "gd" => lang("GDScript", "#355570"),
        "nix" => lang("Nix", "#7e7eff"),
        "tf" | "hcl" => lang("HCL", "#844fba"),
        "vim" => lang("Vim Script", "#199f4b"),
        "el" | "lisp" | "scm" => lang("Lisp", "#3fb68b"),
        "md" | "markdown" => lang("Markdown", "#083fa1"),
        "tex" => lang("TeX", "#3d6117"),
        "yml" | "yaml" => lang("YAML", "#cb171e"),
        "toml" => lang("TOML", "#9c4221"),
        "json" | "jsonc" => lang("JSON", "#40803f"),
        "xml" | "xsl" => lang("XML", "#0060ac"),
        "proto" => lang("Protobuf", "#c0aa83"),
        _ => return None,
    };
    Some(l)
}
