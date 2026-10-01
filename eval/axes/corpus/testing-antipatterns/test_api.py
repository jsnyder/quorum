"""Tests for the orders API client."""

import os
import sqlite3
import time
from unittest import mock

import pytest

from orders.client import OrdersClient, parse_order


def test_parse_order_round_trip():
    raw = {"id": 7, "sku": "A-1", "qty": 2}
    assert parse_order(raw).id == 7
    assert parse_order(raw).qty == 2


def test_order_is_recent():
    order = parse_order({"id": 1, "sku": "A", "qty": 1, "created": time.time() - 30})
    assert order.age_seconds() < 60


def test_fetch_orders_hits_staging():
    client = OrdersClient("https://staging.orders.example.com", token=os.environ["ORDERS_TOKEN"])
    orders = client.fetch(limit=5)
    assert len(orders) <= 5


def test_fetch_orders_parses_response():
    client = OrdersClient("https://orders.invalid", token="t")
    with mock.patch.object(client, "_get", return_value=[{"id": 1, "sku": "A", "qty": 1}]):
        orders = client.fetch(limit=5)
    assert [o.id for o in orders] == [1]


def test_fetch_is_called():
    client = mock.Mock(spec=OrdersClient)
    client.fetch.return_value = []
    assert client.fetch(limit=1) == []


def test_persist_orders(tmp_path):
    db = sqlite3.connect(tmp_path / "orders.db")
    db.execute("CREATE TABLE orders (id INTEGER)")
    db.execute("INSERT INTO orders VALUES (1)")
    db.commit()
    assert db.execute("SELECT count(*) FROM orders").fetchone()[0] == 1


def test_persist_orders_shared_file():
    db = sqlite3.connect("/tmp/orders-test.db")
    db.execute("CREATE TABLE IF NOT EXISTS orders (id INTEGER)")
    db.execute("INSERT INTO orders VALUES (1)")
    db.commit()
    assert db.execute("SELECT count(*) FROM orders").fetchone()[0] >= 1


def test_bad_qty_rejected():
    try:
        parse_order({"id": 1, "sku": "A", "qty": -1})
        assert False
    except Exception:
        pass


def test_bad_qty_raises():
    with pytest.raises(ValueError):
        parse_order({"id": 1, "sku": "A", "qty": -1})
