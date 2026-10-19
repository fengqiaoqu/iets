# Every top-level derivation of nixpkgs, for tests/nixpkgs_eval.rs.
{ nixpkgs, limit ? null }:
let
  pkgs = import nixpkgs { };
  inherit (pkgs) lib;
  isDrv =
    n:
    let
      r = builtins.tryEval (lib.isDerivation pkgs.${n});
    in
    r.success && r.value;
  names = builtins.filter isDrv (builtins.attrNames pkgs);
in
lib.genAttrs (if limit == null then names else lib.take limit names) (n: pkgs.${n})
