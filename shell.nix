{ pkgs ? import <nixpkgs> {
    overlays = [
      (import (builtins.fetchTarball
        "https://github.com/oxalica/rust-overlay/archive/master.tar.gz"))
    ];
  }
}:

let
  # wreq/wreq-util (`domains/scrapers/host/http.rs`) require rustc >=1.98.
  rustToolchain = pkgs.rust-bin.stable."1.98.1".default.override {
    extensions = [ "rust-src" "clippy" "rustfmt" "rust-analyzer" ];
  };
in
pkgs.mkShell {
  packages = with pkgs; [
    rustToolchain
    sccache

    # required by build.rs to compile ../api-protos/*.proto via tonic-prost-build
    protobuf

    # required to build wreq's vendored, patched BoringSSL (btls-sys), used by the
    # scrapers domain's fyde.http (see src/domains/scrapers/host/http.rs) for a real
    # Chrome TLS/HTTP2 fingerprint.
    cmake
    perl
    go
    clang
    # Sets up LIBCLANG_PATH/BINDGEN_EXTRA_CLANG_ARGS correctly for bindgen.
    rustPlatform.bindgenHook

    # required (via pkg-config) to build wry/tao, the real embedded webview
    # behind fyde.browser (see src/domains/scrapers/browser/driver.rs and
    # src/domains/scrapers/host/browser.rs). webkitgtk_4_1/gtk3 are wry's
    # WebKitGTK backend on Linux; dbus is tao's optional single-instance/
    # app-id support, pulled in by its "dbus" feature.
    pkg-config
    webkitgtk_4_1
    gtk3
    dbus
  ];

  # direnv (`use nix`) caches the env from the nix-shell invocation that computed it,
  # including the ephemeral TMPDIR nix-shell creates and deletes on exit. Later shells
  # reusing that cached env point sccache at a TMPDIR that's already gone - pin it.
  shellHook = ''
    export TMPDIR="/tmp"

    export RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache
    export SCCACHE_DIR="$HOME/.cache/sccache"
    export CC="${pkgs.sccache}/bin/sccache ${pkgs.clang}/bin/clang"
    export CXX="${pkgs.sccache}/bin/sccache ${pkgs.clang}/bin/clang++"
  '';
}
