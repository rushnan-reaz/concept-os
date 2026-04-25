build:
	cargo build --release --features uart
	cp ./target/release/update-tool ./update-tool-uart
	chmod +x ./update-tool-uart
	cargo build --release --features mqtt
	cp ./target/release/update-tool ./update-tool-mqtt
	chmod +x ./update-tool-mqtt

bluetooth_connect:
	sudo rfcomm connect /dev/rfcomm0 00:22:09:01:9E:A1 1 
	