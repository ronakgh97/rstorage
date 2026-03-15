Tweaking with TCP UDP QDP because Im bored

Im in need of simple storage server, which can be use in CI pipeline and my other projects...

My initial thoughts

- The protocol will be very simple and secure, ok
- Upload request will contain headers: file-name, file-size, file-hash and a KEY, this key is kind of...lets say a
  master key, nothing much it locks/unlocks the files and return file-Id to client, this file-Id is used to download the
  file later on, and also to delete the file if needed, ok?
- Download request will contain headers: the KEY only for now (I cant think much anything right now and I want to keep
  client simple, just know file-Id and the KEY, if the key is correct, the file will be downloaded, otherwise it will
  return an error and also request path will contain file-Id, so the download request will be like: /download/{file-Id}
- Delete request will be like: /delete/{file-Id} and it will contain the KEY in the header, if the key is correct, the
  file will be deleted, or ongoing task is happening, it will return an error
- Now then What Ifs... If client forgot file-Id or the KEY, well then...good luck :), and all files will be unique cuz
  of file-Id, In the future, I am thinking having file-Id in this format: {6x}-{6x}-{6x}-{6x}, now with PCM, there will
  6 hexadecimal characters, each section with 16^6 combinations, and total of 4 section so 16^24 combinations, so no
  collisions...I hope
- Concurrency, well Thread spawning/Pool will handle that, and I will Dashmap to keep track of the all ops with minimal
  locking, and
  also I will use Tokio for async file ops, so it should be good
- File storage? Of course...Local storage or docker volumes (later)
- Security....*sighhh*..encryption keys I guess...

---

Usage

Use docker

```shell
docker pull ronakgh97/rdrive:latest
docker run -d -p 3000:3000 -v rdrive-storage:/home/rdrive/.rdrive/storage --name rdrive ronakgh97/rdrive:latest
```

Use this client wrapper (You can write your own client, read [Protocol](PROTOCOL.md))

```shell
rdc upload --file dummy.bin --port 3000 --protocol v2        
Enter a lock key: ronak
↪ Starting upload: dummy.bin (1180000000 bytes)
↪ File hash: ef5bfea558b31b8ecf673a0445ec035394f9a3a40fad69cd8a9ad1c5f5aaf56b...
File ID: 2e8e2c5e-9f36-4369-802f-81d6b7fc0e69 - Time took: 3.350482605


rdc download --output . --port 3000 --protocol v2            
File already exists. Do you want to overwrite it? (y/n): y
Enter file ID: 2e8e2c5e-9f36-4369-802f-81d6b7fc0e69
Enter file key: ronak
↩ Downloading: dummy.bin (1180000000 bytes)
Saved to: .\dummy.bin
```

---

TODO

- Encryption keys for storage and metadata
- Bandwidth tracking and limits
- Better Error handling and logging
- Thread pool for better concurrency and resource management
- Better file management and cleanup strategies
- Authentication and access control
- Docker support
- Uhm...what else?
- Fix and improve the buffering and streaming for large files
- More protocol features like file listing, metadata retrieval, more commands etc.
- Graceful shutdown and cleanup
- Little bit client polish
- Protocol v1 meant to be use UDP, but skill issues...
- Encrypted share feature between clients (stateless relay server) without sharing the master key, maybe using some kind
  of temporary keys or
  something, idk

ISSUE

- There is a RACE CONDITION
- HASH MISMATCH UNDER HIGH LOAD, FUCK
- Cross upload/donwload protocol corrupts the server somehow, I don't know, this shouldn't happen Server is stateless