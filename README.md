Tweaking with TCP UDP QDP while building AWS s3 because I'm bored

Use docker [Image](https://hub.docker.com/repository/docker/ronakgh97/rdrive/general)

```shell
docker pull ronakgh97/rdrive:latest (<60 mb)
docker run -d -p 3000:3000 -v rdrive-storage:/home/rdrive/.rdrive/storage --name rdrive ronakgh97/rdrive:latest
```

Then you can use the CLI to push/pull files

```shell
rdrive push --file dummy.bin --port 3000 --protocol v1       
Enter a lock key: ronak
↪ Starting upload: dummy.bin (1180000000 bytes)
↪ File hash: ef5bfea558b31b8ecf673a0445ec035394f9a3a40fad69cd8a9ad1c5f5aaf56b...
File ID: 2e8e2c5e-9f36-4369-802f-81d6b7fc0e69 - Time took: 3.350482605
````

```shell
rdrive pull --output . --port 3000 --protocol v1      
File already exists. Do you want to overwrite it? (y/n): y
Enter file ID: 2e8e2c5e-9f36-4369-802f-81d6b7fc0e69
Enter file key: ronak
↩ Downloading: dummy.bin (1180000000 bytes)
Saved to: .\dummy.bin
```

TODO

- Better Encryption for storage and metadata
- Better Bandwidth tracking and limits
- Better Error handling and logging
- Thread pool for better concurrency and resource management
- Better file management and cleanup strategies
- Authentication and access control
- Uhm...what else?
- Fix and improve the buffering and streaming for large files (diff hashing, chunking, chopping, etc.)
- More protocol features like file listing, metadata retrieval, more commands etc.
- Graceful shutdown and cleanup
- Little bit client polish
- Protocol v2 meant to be use UDP, but skill issues...
- Encrypted share feature between clients (stateless relay server) without sharing the master key, maybe using some kind
  of temporary keys or
  something, idk
- Migrate to async architecture (TOKIO)
- Too many repetitive code, need to refactor and clean up the codebase
- Still some buffering issues, data gets stalls, does not flush properly
- Multi-port support for better concurrency
- rsync support (rolling hashing, delta transfers, etc.)

ISSUE

- There is a RACE CONDITION
- HASH MISMATCH UNDER HIGH LOAD, FUCK
- Cross upload/donwload protocol corrupts the server somehow, I don't know, this shouldn't happen Server is stateless