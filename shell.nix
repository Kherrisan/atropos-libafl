let
  pkgs = import <nixpkgs> { system = "x86_64-linux"; };
  legacyCurl = pkgs.curl.override { openssl = pkgs.openssl_1_1; };
in
pkgs.mkShell {
  shellHook = ''
    export ATROPOS_NIX_BZIP2_DEV="${pkgs.bzip2.dev}"
    export ATROPOS_NIX_BZIP2_LIB="${pkgs.bzip2.out}"
    export ATROPOS_NIX_OPENSSL_DEV="${pkgs.openssl_1_1.dev}"
    export ATROPOS_NIX_OPENSSL_LIB="${pkgs.openssl_1_1.out}"
  '';
  packages = with pkgs; [
    autoconf
    bison
    patch
    re2c
    pkg-config
  ];
  buildInputs = with pkgs; [
    libxml2
    sqlite
    legacyCurl
    openssl_1_1
    readline
    oniguruma
    libzip
    zlib
    libpng
    libjpeg
    libwebp
    freetype
    icu
    libxslt
    bzip2
    gmp
  ];
}
