# A fake stdenv: enough for nix-shell and nix develop.
let
  inherit (import ./config.nix) mkDerivation path;
  stdenv = mkDerivation {
    name = "stdenv";
    buildCommand = ''
      mkdir $out
      echo 'export PATH=${path}' > $out/setup
      echo 'genericBuild() { eval "$buildCommand"; }' >> $out/setup
    '';
  };
in
mkDerivation {
  name = "shell";
  # get-env.sh writes to each of these, as stdenv sets it
  outputs = [ "out" ];
  inherit stdenv;
  builder = builtins.toFile "builder.sh" ''
    source $stdenv/setup
    genericBuild
  '';
  FOO = "from-shell";
  buildCommand = "echo $FOO > $out";
}
