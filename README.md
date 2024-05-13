Compiling:  
install necessary toolchains:  
install [rustup](https://www.rust-lang.org/learn/get-started) or use your package manager (preferred)  
`rustup toolchain install nightly` (this project uses the nightly toolchain as one unstable feature is used)  
`rustup target add aarch64-unknown-linux-gnu` (add the right target of the rustc compiler)  
`cargo install cargo-zigbuild` (tool to link using the zig compiler, requires the zig compiler to be installed)  
Install [zig](https://ziglang.org/learn/getting-started/) allows for linking any glibc version that is desired and means you dont have to set up link arguments to build.  
get targets glibc version (`ldd --version`)


`cargo zigbuild --target aarch64-unknown-linux-gnu.2.*minor glibc version* --release`  
for example for the Moduline controllers:  
`cargo zigbuild --target aarch64-unknown-linux-gnu.2.31 --release`
