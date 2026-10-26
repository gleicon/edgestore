#!/bin/bash
# MinIO init script — creates the S3 bucket used by EdgeStore integration tests.
# Run after the MinIO container is ready.

set -e

ENDPOINT="${EDGESTORE_S3_ENDPOINT_URL:-http://localhost:9000}"
BUCKET_NAME="${EDGESTORE_S3_BUCKET:-edgestore-test}"

echo "[minio-init] Creating S3 bucket: $BUCKET_NAME at $ENDPOINT"
aws s3 mb "s3://$BUCKET_NAME" \
    --endpoint-url "$ENDPOINT" \
    --region us-east-1

echo "[minio-init] Done."
