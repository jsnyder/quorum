import sqlite3
import psycopg2
import mysql.connector

# TP: should match
conn = sqlite3.connect("app.db")  # ruleid: db-connection-no-context-manager
conn2 = psycopg2.connect("dbname=test")  # ruleid: db-connection-no-context-manager
conn3 = mysql.connector.connect(user='scott')  # ruleid: db-connection-no-context-manager

# FP: should NOT match
with sqlite3.connect("app.db") as conn4:  # ok: db-connection-no-context-manager
    pass

with psycopg2.connect("dbname=test") as conn5:  # ok: db-connection-no-context-manager
    pass
