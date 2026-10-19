let
  inherit (import ./config.nix) mkDerivation;
in
{
  # sha256 of "fixed\n"
  flat = mkDerivation {
    name = "fixed";
    outputHashMode = "flat";
    outputHashAlgo = "sha256";
    outputHash = "sha256-DDBxQY5jVuYUiYyE7QZMqV6IVRvAgRtTS98ZUuza5TQ=";
    buildCommand = "echo fixed > $out";
  };
}
