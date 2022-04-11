# Rust_Pinecone
This is a port of the peer-to-peer overlay routing 
mechanism [Pinecone](https://github.com/matrix-org/pinecone) 
and aims to be interoperable with it, although it isn't yet because 
at a minimum frame signing is missing. 
It is still very much work in progress but at least halfway functional. 
There are still a bunch of unhandled error cases that will just panic 
for now. 


## Compile from source
```shell
$ cargo build
```

## Usage
```shell
$ ./target/debug/rust_pinecone
[INFO  rust_pinecone] Router [69,56,205,245,126,111,40,254,20,177,36,26,76,121,47,208,76,24,131,161,83,1,123,54,82,171,139,176,144,107,40,110]
[INFO  rust_pinecone] Listening on [::]:44995
Available actions:
1) Add peer
2) Send message

```
Optionally add a listen address as an argument. 
The default is `127.0.0.1:0` which assigns a random listen port.
```shell
$ ./target/debug/rust_pinecone '0.0.0.0:0'
  # or 
$ ./target/debug/rust_pinecone '[::]:0'
```
Add a peer
```shell
Available actions:
1) Add peer
2) Send message
1
Address of peer:
127.0.0.1:37301
```
Send a demo message to a peer
```shell
Available actions:
1) Add peer
2) Send message
2
Target key:
[131,221,229,133,52,62,223,119,250,217,34,60,39,203,108,100,35,178,32,49,78,138,249,102,204,127,102,132,223,207,90,201]
Message:
Hello router
```
The message will then be routed automagically 
to the node with the target key.

