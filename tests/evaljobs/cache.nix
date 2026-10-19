# --check-cache-status shapes: a chain that needs building (issue #369), a
# job whose output is in the local store, and a fixed-output derivation.
{
  system ? "x86_64-linux",
}:
rec {
  nginx = derivation {
    name = "nginx-1.24.0";
    inherit system;
    builder = "/bin/sh";
    args = [ "-c" "echo 'content' > $out" ];
  };
  proxyWrapper = derivation {
    name = "proxyWrapper";
    system = "aarch64-linux";
    builder = "/bin/sh";
    args = [ "-c" "echo '${nginx}' > $out" ];
  };
  webService = derivation {
    name = "webService";
    system = "aarch64-linux";
    builder = "/bin/sh";
    args = [ "-c" "echo '${proxyWrapper}' > $out" ];
    outputs = [ "out" "doc" ];
  };
  fod = derivation {
    name = "fod";
    inherit system;
    builder = "/bin/sh";
    args = [ "-c" "echo hi > $out; : ${nginx}" ];
    outputHashMode = "flat";
    outputHashAlgo = "sha256";
    outputHash = "98ea6e4f216f2fb4b69fff9b3a44842c38686ca685f3f55dc48c5d3fb1107be4";
  };
  # A nixpkgs package whose output may be in the store of the box that
  # runs the tests.
  hello = (import <nixpkgs> { inherit system; }).hello;
}
