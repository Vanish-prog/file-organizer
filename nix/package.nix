{ lib, stdenv, rustPlatform, pkg-config, clang, cmake, makeWrapper, makeDesktopItem,
  copyDesktopItems, libclang, python3, findmatch, runtimeLibraries }:

rustPlatform.buildRustPackage {
  pname = "file-organizer";
  version = "0.1.0";
  src = lib.cleanSource ../.;
  cargoHash = "sha256-Zd4n4s0iB/9x0DwVaGLDJeWQagJI9lEB7MODCEhf2Oc=";
  nativeBuildInputs = [ pkg-config clang cmake makeWrapper copyDesktopItems ];
  nativeCheckInputs = [ python3 ];
  buildInputs = runtimeLibraries;
  LIBCLANG_PATH = "${libclang.lib}/lib";
  FILE_ORGANIZER_FINDMATCH = "${findmatch}/bin/findmatch";
  LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibraries;
  # Shell fixtures need Nix's bash path rather than a nonexistent /bin/sh.
  preCheck = ''
    substituteInPlace src/search.rs --replace-fail '#!/bin/sh' '#!${stdenv.shell}'
  '';
  desktopItems = [ (makeDesktopItem {
    name = "file-organizer";
    desktopName = "File Organizer";
    comment = "Review and organize photos and files locally";
    exec = "file-organizer %f";
    terminal = false;
    categories = [ "Utility" "FileTools" ];
    mimeTypes = [ "inode/directory" ];
  }) ];
  postInstall = ''
    wrapProgram "$out/bin/file-organizer" \
      --set FILE_ORGANIZER_FINDMATCH "${findmatch}/bin/findmatch" \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibraries}"
  '';
  meta = {
    description = "Native GPUI photo-first file organizer with pinned local findmatch";
    mainProgram = "file-organizer";
    platforms = [ "x86_64-linux" ];
    # No application license has been selected; do not invent one.
  };
}
