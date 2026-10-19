# A chain of drvs, each referring to the one before it.
{ n ? 4 }:
let
  inherit (import ./config.nix) mkDerivation;
  step =
    i:
    mkDerivation {
      name = "chain-${toString i}";
      dep = if i == 0 then "" else step (i - 1);
      buildCommand = ''
        mkdir $out
        echo ${toString i} > $out/n
        if [ -n "$dep" ]; then ln -s $dep $out/dep; fi
      '';
    };
in
step (n - 1)
