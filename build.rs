use std::process::Command;

fn php_config(arg: &str) -> String {
    let output = Command::new("/opt/atropos-embed/bin/php-config")
        .arg(arg)
        .output()
        .expect("php-config");
    assert!(output.status.success(), "php-config {arg} failed");
    String::from_utf8(output.stdout).unwrap()
}

fn main() {
    let prefix = php_config("--prefix");
    let prefix = prefix.trim();
    let includes = php_config("--includes");
    let mut build = cc::Build::new();
    build.file("harness/libatropos.c");
    build.include(format!("{prefix}/include/php/sapi/embed"));
    for token in includes.split_whitespace() {
        if let Some(path) = token.strip_prefix("-I") {
            build.include(path);
        }
    }
    build.compile("atropos_harness");

    println!("cargo:rerun-if-changed=harness/libatropos.c");
    println!("cargo:rustc-link-search=native={prefix}/lib");
    println!("cargo:rustc-link-lib=php7");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}/lib");
    for token in php_config("--libs").split_whitespace() {
        if let Some(lib) = token.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={lib}");
        } else if let Some(dir) = token.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={dir}");
        }
    }
}
