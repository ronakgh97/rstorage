## Protocol v2: Custom TCP (Recommended)

A lightweight, high-performance protocol over TCP with text headers and binary body.

### Request/Response Format

```
<command>\n
<key>: <value>\n
<key>: <value>\n
\n
<binary body (optional)>
```

- Headers end with `\n\n` (double newline)
- After headers, optionally contains binary data

---

### UPLOAD

#### Request

```
UPLOAD
file-name: <filename>
file-size: <size in bytes>
file-hash: <sha256 hash>
file-key: <secret key>

<binary data>
```

#### Response (Success)

```
OK
file-id: <uuid>
time-took: <seconds>
\n\n
```

#### Response (Error)

```
ERROR
code: <http status code>
message: <error message>
\n\n
```

---

### DOWNLOAD

#### Request

```
DOWNLOAD
file-id: <uuid>
file-key: <secret key>
\n\n
```

#### Response

```
file-name: <filename>
file-size: <size in bytes>
file-hash: <sha256 hash>

<binary data>
```

### STATUS

#### Request

```
STATUS
```

#### Response

```
OK
code: 200
timestamp: <rfc3339>
uptime_hrs: <hours>
no_goings_task: <count>
total_connections: <count>
total_bandwidth_gb: <gb>
```

---

### Example: Upload

```python
# Python example
import socket
import hashlib

# Read file and compute hash/size
with open("file.bin", "rb") as f:
    data = f.read()
file_hash = hashlib.sha256(data).hexdigest()
file_size = len(data)

# Connect and send
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect(("localhost", 3000))

request = f"UPLOAD\nfile-name: file.bin\nfile-size: {file_size}\nfile-hash: {file_hash}\nfile-key: secret\n\n"
sock.sendall(request.encode())

# Send binary
sock.sendall(data)
sock.shutdown(socket.SHUT_WR)

# Read response headers until double newline
headers_buf = b""
while b"\n\n" not in headers_buf:
    chunk = sock.recv(4096)
    if not chunk:
        break
    headers_buf += chunk

header_part, _, _ = headers_buf.partition(b"\n\n")
lines = header_part.split(b"\n")
status = lines[0].decode() if lines else ""
headers = {}
for line in lines[1:]:
    if b":" in line:
        k, v = line.split(b":", 1)
        headers[k.strip().decode()] = v.strip().decode()

if status == "OK":
    print("uploaded:", headers.get("file-id"), "time-took:", headers.get("time-took"))
elif status == "ERROR":
    print("error:", headers.get("code"), headers.get("message"))
else:
    print("unexpected response")
```

### Example: Download

```python
import socket
import hashlib

sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect(("localhost", 3000))

request = "DOWNLOAD\nfile-id: abc-123\nfile-key: secret\n\n"
sock.sendall(request.encode())

# Read headers
headers_buf = b""
while b"\n\n" not in headers_buf:
    chunk = sock.recv(4096)
    if not chunk:
        break
    headers_buf += chunk

header_part, _, body_start = headers_buf.partition(b"\n\n")
headers = {}
for line in header_part.split(b"\n"):
    if b":" in line:
        k, v = line.split(b":", 1)
        headers[k.strip().decode()] = v.strip().decode()

file_size = int(headers.get("file-size", "0"))
file_name = headers.get("file-name", "download.bin")
expected_hash = headers.get("file-hash")

# Read binary data based on file-size
remaining = file_size - len(body_start)
hasher = hashlib.sha256()
with open(file_name, "wb") as f:
    if body_start:
        f.write(body_start)
        hasher.update(body_start)
    while remaining > 0:
        chunk = sock.recv(min(65536, remaining))
        if not chunk:
            break
        f.write(chunk)
        hasher.update(chunk)
        remaining -= len(chunk)

sock.close()

if expected_hash:
    received_hash = hasher.hexdigest()
    print("hash match:", received_hash == expected_hash)
else:
    print("download complete:", file_name)
```

---

## Protocol v1: TCP with Length-Prefixed Framing

A lightweight TCP protocol using length-prefixed message framing. Reliable and simple.

### Message Format

```
[2 bytes: payload length (u32, big-endian)][payload]

- Payload contains: command + headers + optional binary data
- Headers end with single newline
```

### UPLOAD

#### Request

```
[2-byte length][UPLOAD
file-name: <filename>
file-size: <size in bytes>
file-hash: <sha256 hash>
file-key: <secret key>

<binary data>]
```

#### Response (Success)

```
[2-byte length][OK
file-id: <uuid>
file-key: <secret key>
time-took: <seconds>]
```

#### Response (Error)

```
[2-byte length][ERROR
code: <http status code>
message: <error message>]
```

---

### DOWNLOAD

#### Request

```
[2-byte length][DOWNLOAD
file-id: <uuid>
file-key: <secret key>]
```

#### Response

```
[2-byte length][OK
file-name: <filename>
file-size: <size in bytes>
file-hash: <sha256 hash>]

<binary data>
```

---

### Python Example: Upload

```python
import socket
import hashlib
import struct


def read_frame(sock):
    # Read 4-byte length prefix
    length_data = sock.recv(4)
    if len(length_data) < 4:
        return None
    length = struct.unpack(">I", length_data)[0]

    # Read payload
    payload = b""
    while len(payload) < length:
        chunk = sock.recv(length - len(payload))
        if not chunk:
            break
        payload += chunk
    return payload


def send_frame(sock, data):
    # Send 4-byte length prefix + payload
    length = struct.pack(">I", len(data))
    sock.sendall(length + data)


# Read file and compute hash
filename = "file.bin"
with open(filename, "rb") as f:
    data = f.read()
file_size = len(data)
file_hash = hashlib.sha256(data).hexdigest()

# Connect to server
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect(("localhost", 3000))

# Build request
request = f"UPLOAD\nfile-name: {filename}\nfile-size: {file_size}\nfile-hash: {file_hash}\nfile-key: secret\n".encode()
send_frame(sock, request)

# Send file data
send_frame(sock, data)

# Read response
response = read_frame(sock)
if response:
    response = response.decode()
    if response.startswith("OK\n"):
        lines = response.split("\n")
        file_id = None
        file_key = None
        for line in lines:
            if line.startswith("file-id: "):
                file_id = line.split(": ", 1)[1]
            if line.startswith("file-key: "):
                file_key = line.split(": ", 1)[1]
        print(f"Uploaded! file_id: {file_id}, file_key: {file_key}")
    else:
        print(f"Error: {response}")
```

### Python Example: Download

```python
import socket
import hashlib
import struct


def read_frame(sock):
    length_data = sock.recv(4)
    if len(length_data) < 4:
        return None
    length = struct.unpack(">I", length_data)[0]

    payload = b""
    while len(payload) < length:
        chunk = sock.recv(length - len(payload))
        if not chunk:
            break
        payload += chunk
    return payload


def send_frame(sock, data):
    length = struct.pack(">I", len(data))
    sock.sendall(length + data)


# Connect to server
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect(("localhost", 3000))

# Send download request
file_id = "abc-123"
file_key = "secret"
request = f"DOWNLOAD\nfile-id: {file_id}\nfile-key: {file_key}\n".encode()
send_frame(sock, request)

# Read response header
header = read_frame(sock)
if header:
    header = header.decode()
    if header.startswith("ERROR"):
        print(f"Error: {header}")
    else:
        # Parse headers
        lines = header.split("\n")
        filename = file_id
        file_size = 0
        expected_hash = None

        for line in lines:
            if line.startswith("file-name: "):
                filename = line.split(": ", 1)[1]
            elif line.startswith("file-size: "):
                file_size = int(line.split(": ", 1)[1])
            elif line.startswith("file-hash: "):
                expected_hash = line.split(": ", 1)[1]

        print(f"Downloading: {filename} ({file_size} bytes)")

        # Read file data
        received = 0
        hasher = hashlib.sha256()
        with open(filename, "wb") as f:
            while received < file_size:
                chunk = sock.recv(min(8 * 1024 * 1024, file_size - received))
                if not chunk:
                    break
                f.write(chunk)
                hasher.update(chunk)
                received += len(chunk)

        # Verify hash
        actual_hash = hasher.hexdigest()
        if actual_hash == expected_hash:
            print(f"Hash verified! ({actual_hash})")
        else:
            print(f"Hash mismatch! expected: {expected_hash}, got: {actual_hash}")
```

---

## Security

**ChaCha128** encrypts file metadata at rest.

- Key: 256-bit, auto-generated or set via `MASTER_KEY` env var
- Nonce: 96-bit, random per encryption
- 128 diffusion rounds
- Checkout implementation: ![Impl](src/lib.rs)

Files stored as:

- `~/.rdrive/storage/<file_id>` - raw file data
- `~/.rdrive/storage/<file_id>.meta` - encrypted metadata (nonce || ciphertext)

NOTE: raw-file isnt encrypyted
