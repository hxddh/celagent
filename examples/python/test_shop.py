"""A tiny "shop" against local AWS: the example Lenslet's own CI runs.

    lenslet run --update -- python test_shop.py   # writes lenslet.lock
    lenslet run --check  -- python test_shop.py   # fails if access grows

Needs boto3 and an emulator (AWS Emulator, or `moto_server -p 9298` with
`--upstream http://127.0.0.1:9298`).
"""

import json
import os
import uuid

import boto3

REGION = "us-east-1"
s3 = boto3.client("s3", region_name=REGION)
ddb = boto3.client("dynamodb", region_name=REGION)
sqs = boto3.client("sqs", region_name=REGION)


def setup():
    """Fixtures: the tests create what the app expects to exist."""
    s3.create_bucket(Bucket="shop-receipts")
    ddb.create_table(
        TableName="orders",
        KeySchema=[{"AttributeName": "id", "KeyType": "HASH"}],
        AttributeDefinitions=[{"AttributeName": "id", "AttributeType": "S"}],
        BillingMode="PAY_PER_REQUEST",
    )
    return sqs.create_queue(QueueName="fulfilment")["QueueUrl"]


def place_order(queue_url, total):
    """The application code under test."""
    order_id = str(uuid.uuid4())
    ddb.put_item(
        TableName="orders",
        Item={"id": {"S": order_id}, "total": {"N": str(total)}, "status": {"S": "paid"}},
    )
    receipt = json.dumps({"order": order_id, "total": total}).encode()
    s3.put_object(Bucket="shop-receipts", Key=f"receipts/{order_id}.json", Body=receipt)
    sqs.send_message(QueueUrl=queue_url, MessageBody=json.dumps({"order": order_id}))
    if os.environ.get("SHOP_REFUNDS"):
        # A change under review: refunds delete the order. New access.
        ddb.delete_item(TableName="orders", Key={"id": {"S": order_id}})
    return order_id


def test():
    queue_url = setup()
    order_id = place_order(queue_url, 42.5)
    if not os.environ.get("SHOP_REFUNDS"):
        item = ddb.get_item(TableName="orders", Key={"id": {"S": order_id}})["Item"]
        assert item["status"]["S"] == "paid"
    body = s3.get_object(Bucket="shop-receipts", Key=f"receipts/{order_id}.json")["Body"].read()
    assert json.loads(body)["total"] == 42.5
    print("ok: order", order_id)


if __name__ == "__main__":
    test()
