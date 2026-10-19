# nix-grpc-daemon, the gRPC build server that tests/e2e_grpc.rs runs
# against: the daemon only, built with the project's own package files
# against this nixpkgs' Nix. Its flake inputs (niks3, a Nix master pin)
# serve its farm mode and its own tests, which the test does not use.
{ pkgs }:
let
  # fetched at evaluation, not by a derivation: the package files are
  # imported from it, and a check may not import from a derivation
  src = builtins.fetchTarball {
    url = "https://github.com/Mic92/nix-grpc-store/archive/4f6d239e834f96bd1544a8b243d46c41a892016e.tar.gz";
    sha256 = "sha256-JNwiqh3oaU0uToxtK+rf3FxqpoGnH/hCh7Q+QFSfcwk=";
  };
  jwt-cpp = pkgs.callPackage "${src}/nix/packages/jwt-cpp.nix" { };
  plugin = pkgs.callPackage "${src}/nix/packages/plugin.nix" {
    inherit jwt-cpp;
    inherit (pkgs.nix.libs) nix-store nix-util;
  };
in
plugin.overrideAttrs {
  pname = "nix-grpc-daemon";
  ninjaFlags = [ "nix-grpc-daemon" ];
  doCheck = false;
  separateDebugInfo = false;
  installPhase = ''
    runHook preInstall
    install -Dm755 nix-grpc-daemon $out/bin/nix-grpc-daemon
    runHook postInstall
  '';
}
