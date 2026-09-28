let
  pkgs = import <nixpkgs> { system = "x86_64-linux"; };
in
pkgs.mkShell {
  packages = with pkgs; [
    autoconf
    automake
    bison
    cpio
    cdrkit
    cloud-utils
    curl
    e2fsprogs
    git
    flex
    gnumake
    gnutar
    gzip
    libtool
    meson
    mariadb
    nim
    ninja
    pax-utils
    perl
    patch
    pkg-config
    python3
    python3Packages.jinja2
    python3Packages.msgpack
    python3Packages.setuptools
    qemu-utils
    ripgrep
    re2c
    xz
  ];
  buildInputs = with pkgs; [
    glib.dev
    pixman
    zlib.dev
    libaio
    libcap.dev
    numactl
    libxml2.dev
    libffi.dev
    gnutls.dev
    libgcrypt.dev
    libgpg-error.dev
    libslirp
    liburing.dev
    libseccomp.dev
    lzo
    zstd.dev
    libusb1.dev
    usbredir.dev
    ncurses.dev
    libxml2
    sqlite
    openssl_1_1
    readline
    oniguruma
    libzip
    libpng
    libjpeg
    libwebp
    freetype
    icu
    libxslt
    bzip2
    gmp
  ];
  shellHook = ''
    export ATROPOS_NIX_BZIP2_DEV="${pkgs.bzip2.dev}"
    export ATROPOS_NIX_BZIP2_LIB="${pkgs.bzip2.out}"
    export ATROPOS_NIX_OPENSSL_DEV="${pkgs.openssl_1_1.dev}"
    export ATROPOS_NIX_OPENSSL_LIB="${pkgs.openssl_1_1.out}"
  '';
}
