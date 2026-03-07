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
- Concurrency, well Axum will handle that, and I will Dashmap to keep track of the all ops with minimal locking, and
  also I will use Tokio for async file ops, so it should be good
- File storage? Of course...Local storage or docker volumes (later)
- Security....*sighhh*..encryption keys I guess...