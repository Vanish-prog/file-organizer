{
  description = "Native GPUI photo-first file organizer";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    findmatch.url = "github:Vanish-prog/search-tool";
  };
  outputs = { self, nixpkgs, findmatch }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs { inherit system; };
    findmatchPackage = findmatch.packages.${system}.default;
    libs = with pkgs; [ libxkbcommon wayland vulkan-loader libGL fontconfig freetype openssl zlib libx11 libxcb libxcursor libxi libxrandr ];
    file-organizer = pkgs.callPackage ./nix/package.nix {
      findmatch = findmatchPackage;
      runtimeLibraries = libs;
    };
  in {
    packages.${system} = { inherit file-organizer; default = file-organizer; };
    apps.${system}.default = {
      type = "app";
      program = "${file-organizer}/bin/file-organizer";
      meta.description = "Native photo-first file organizer";
    };
    checks.${system}.default = file-organizer;
    devShells.${system}.default = pkgs.mkShell {
      packages = (with pkgs; [ cargo rustc rustfmt clippy pkg-config clang cmake makeWrapper ]) ++ libs ++ [ findmatchPackage ];
      FILE_ORGANIZER_FINDMATCH = "${findmatchPackage}/bin/findmatch";
      LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
      LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;
      RUST_BACKTRACE = "1";
    };
  };
}
