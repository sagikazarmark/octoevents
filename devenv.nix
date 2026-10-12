{ pkgs, ... }:

{
  packages = with pkgs; [
    just

    lld
    cargo-audit
    cargo-deny
    cargo-hack
    cargo-nextest
    cargo-release
    cargo-watch
  ];

  languages = {
    rust = {
      enable = true;
      channel = "stable";
      targets = [ "wasm32-unknown-unknown" ];
    };
  };
}
