#!/bin/bash
set -e

IMAGE_NAME="rdrive:test"
NETWORK="rdrive-test-net"
GARBAGE_SIZE_MB=64

cleanup() {
    echo "Cleaning up"
    docker rm -f rdrive-server-v1 rdrive-server-v2 2>/dev/null || true
    docker network rm $NETWORK 2>/dev/null || true
}

trap cleanup EXIT

docker network create $NETWORK 2>/dev/null || true

docker build -f Dockerfile.test -t $IMAGE_NAME .

# Run cargo tests
docker run --rm --network $NETWORK $IMAGE_NAME cargo test --release --test test_protocol

# Start servers
echo "Starting v1 server"
docker run -d --name rdrive-server-v1 --network $NETWORK $IMAGE_NAME \
  /app/target/release/rdrive-server serve --protocol v1 --port 3000

echo "Starting v2 server"
docker run -d --name rdrive-server-v2 --network $NETWORK $IMAGE_NAME \
  /app/target/release/rdrive-server serve --protocol v2 --port 3001

sleep 3

# Generate random test file inside container
TEST_FILE="/tmp/test_garbage.bin"
echo "Generating ${GARBAGE_SIZE_MB}MB random test file ==="
docker exec rdrive-server-v1 sh -c "dd if=/dev/urandom of=$TEST_FILE bs=1M count=$GARBAGE_SIZE_MB 2>/dev/null"
ORIGINAL_HASH=$(docker exec rdrive-server-v1 md5sum $TEST_FILE | awk '{print $1}')
echo "Original hash: $ORIGINAL_HASH"

# Test v1
echo "Testing v1 client binary upload"
FILE_ID=$(docker exec rdrive-server-v1 /app/target/release/rdrive \
  upload --file $TEST_FILE --port 3000 --protocol v1 --file-key testkey123 2>&1)
echo "v1 upload output: $FILE_ID"

# Extract file-id from output
FILE_ID=$(echo "$FILE_ID" | grep -o '[a-f0-9-]\{36\}' | head -1)
echo "v1 file-id: $FILE_ID"

echo "Testing v1 client binary download"
docker exec rdrive-server-v1 /app/target/release/rdrive \
  download --file-id "$FILE_ID" --file-key testkey123 --port 3000 --protocol v1 --output /tmp/test_download.bin 2>&1

# Verify checksum
DOWNLOAD_HASH=$(docker exec rdrive-server-v1 md5sum /tmp/test_download.bin | awk '{print $1}')
echo "v1 downloaded hash: $DOWNLOAD_HASH"

if [ "$ORIGINAL_HASH" != "$DOWNLOAD_HASH" ]; then
    echo "v1 FAILED: hash mismatch"
    exit 1
fi
echo "v1 passed!"

# Test v2
echo "Testing v2 client binary upload"
FILE_ID=$(docker exec rdrive-server-v2 /app/target/release/rdrive \
  upload --file $TEST_FILE --port 3001 --protocol v2 --file-key testkey123 2>&1)
echo "v2 upload output: $FILE_ID"

FILE_ID=$(echo "$FILE_ID" | grep -o '[a-f0-9-]\{36\}' | head -1)
echo "v2 file-id: $FILE_ID"

echo "Testing v2 client binary download"
docker exec rdrive-server-v2 /app/target/release/rdrive \
  download --file-id "$FILE_ID" --file-key testkey123 --port 3001 --protocol v2 --output /tmp/test_download.bin 2>&1

DOWNLOAD_HASH=$(docker exec rdrive-server-v2 md5sum /tmp/test_download.bin | awk '{print $1}')
echo "v2 downloaded hash: $DOWNLOAD_HASH"

if [ "$ORIGINAL_HASH" != "$DOWNLOAD_HASH" ]; then
    echo "v2 FAILED: hash mismatch"
    exit 1
fi
echo "v2 passed!"

echo "All tests passed"
