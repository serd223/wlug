# Doesn't build plug5 because linker options on Windows is a pain
cd plugs\plug1
cargo build --release --target=wasm32-unknown-unknown
cp .\target\wasm32-unknown-unknown\release\plug1.wasm ..\..\
cd ..\..
cd plugs\plug2
cargo build --release --target=wasm32-unknown-unknown
cp .\target\wasm32-unknown-unknown\release\plug2.wasm ..\..\
cd ..\..
cd plugs\plug3
cargo build --release --target=wasm32-unknown-unknown
cp .\target\wasm32-unknown-unknown\release\plug3.wasm ..\..\
cd ..\..
cd plugs\plug4
cargo build --release --target=wasm32-unknown-unknown
cp .\target\wasm32-unknown-unknown\release\plug4.wasm ..\..\
cd ..\..


