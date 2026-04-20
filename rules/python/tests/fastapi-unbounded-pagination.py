# Fixture: fastapi-unbounded-pagination
from fastapi import APIRouter, Query

router = APIRouter()

# match: unbounded bare-int limit/offset on GET handler
@router.get("/items")
async def list_items(limit: int, offset: int):
    return []

# match: defaulted int without Query bounds
@router.get("/pages")
async def list_pages(page: int = 1):
    return []

# no-match: bounded via Query with le=
@router.get("/items_bounded")
async def list_items_bounded(limit: int = Query(100, le=500)):
    return []

# no-match: non-route helper
def helper(limit: int):
    return limit
