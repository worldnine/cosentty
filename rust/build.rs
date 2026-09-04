use std::{env, error::Error, fs, path::PathBuf};

use syntect::parsing::SyntaxDefinition;

fn main() -> Result<(), Box<dyn Error>> {
    const MARKDOWN: &str = "assets/syntaxes/Markdown.sublime-syntax";
    println!("cargo:rerun-if-changed={MARKDOWN}");

    // two-face 0.5.2 carries bat 0.26.1's older Markdown grammar. Append the
    // current Sublime definition (reverse lookup makes the last one win), then
    // serialize once at build time so TUI startup does not parse 126 KiB YAML.
    let source = fs::read_to_string(MARKDOWN)?;
    let markdown = SyntaxDefinition::load_from_str(&source, true, Some("Markdown"))?;
    let mut builder = two_face::syntax::extra_newlines().into_builder();
    builder.add(markdown);
    let syntaxes = builder.build();

    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is missing")?)
        .join("cosense-syntaxes.packdump");
    syntect::dumps::dump_to_uncompressed_file(&syntaxes, out)?;
    Ok(())
}
