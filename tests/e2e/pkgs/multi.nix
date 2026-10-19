let
  inherit (import ./config.nix) mkDerivation;
in
mkDerivation {
  name = "multi";
  outputs = [ "out" "dev" "doc" ];
  buildCommand = ''
    echo out > $out
    echo dev $out > $dev
    echo doc > $doc
  '';
}
