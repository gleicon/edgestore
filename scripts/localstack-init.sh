#!/bin/bash
# LocalStack init script — runs after the container is ready.
# Creates the S3 bucket used by EdgeStore integration tests.

set -e

BUCKET_NAME="${EDGESTORE_S3_BUCKET:-edgestore-test}"

echo "[localstack-init] Creating S3 bucket: $BUCKET_NAME"
awslocal s3 mb "s3://$BUCKET_NAME"

echo "[localstack-init] Done."
