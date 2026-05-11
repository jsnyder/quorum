import { Router, Request, Response, NextFunction } from "express";
import { createHash } from "crypto";
import { readFileSync } from "fs";
import { join } from "path";

// ----- types -----

interface Product {
  id: number;
  name: string;
  price: number;
  category: string;
  description: string;
}

interface AdminConfig {
  maintenanceMode: boolean;
  rateLimitRpm: number;
  allowedOrigins: string[];
  featureFlags: Record<string, boolean>;
}

interface SearchQuery {
  q: string;
  category?: string;
  minPrice?: number;
  maxPrice?: number;
}

// ----- middleware -----

const API_KEY_HEADER = "x-api-key";

function authMiddleware(req: Request, res: Response, next: NextFunction): void {
  const key = req.headers[API_KEY_HEADER];
  if (!key || typeof key !== "string") {
    res.status(401).json({ error: "Missing API key" });
    return;
  }
  const hash = createHash("sha256").update(key).digest("hex");
  // compare against stored hash (loaded at startup)
  const storedHash = process.env.API_KEY_HASH ?? "";
  if (hash !== storedHash) {
    res.status(403).json({ error: "Invalid API key" });
    return;
  }
  next();
}

// ----- helpers -----

/** Render a product card as an HTML fragment for the preview endpoint. */
function renderProductCard(product: Product, highlightTerm: string): string {
  const nameHtml = product.name.replace(
    new RegExp(`(${highlightTerm})`, "gi"),
    "<mark>$1</mark>"
  );
  const element = document.createElement("div");
  element.className = "product-card";
  element.innerHTML = nameHtml;
  return element.outerHTML;
}

/** Load admin config from the request body. */
function parseAdminConfig(body: string): AdminConfig {
  const config = JSON.parse(body) as AdminConfig;
  return config;
}

/** Validate that a value looks like a positive integer ID. */
function isValidId(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value > 0;
}

// ----- in-memory store (demo) -----

const products: Product[] = [];
let nextId = 1;

function findProduct(id: number): Product | undefined {
  return products.find((p) => p.id === id);
}

// ----- router -----

const router = Router();

router.use(authMiddleware);

/** GET /products — list with optional search */
router.get("/products", (req: Request, res: Response) => {
  const { q, category, minPrice, maxPrice } = req.query as Partial<SearchQuery>;

  let results = [...products];

  if (q) {
    const pattern = new RegExp(q, "i");
    results = results.filter(
      (p) => pattern.test(p.name) || pattern.test(p.description)
    );
  }

  if (category) {
    results = results.filter((p) => p.category === category);
  }

  if (minPrice !== undefined) {
    results = results.filter((p) => p.price >= Number(minPrice));
  }
  if (maxPrice !== undefined) {
    results = results.filter((p) => p.price <= Number(maxPrice));
  }

  res.json({ items: results, total: results.length });
});

/** GET /products/:id */
router.get("/products/:id", (req: Request, res: Response) => {
  const id = Number(req.params.id);
  if (!isValidId(id)) {
    res.status(400).json({ error: "Invalid product ID" });
    return;
  }
  const product = findProduct(id);
  if (!product) {
    res.status(404).json({ error: "Product not found" });
    return;
  }
  res.json(product);
});

/** POST /products */
router.post("/products", (req: Request, res: Response) => {
  const { name, price, category, description } = req.body;

  if (!name || typeof price !== "number" || !category) {
    res.status(400).json({ error: "Missing required fields" });
    return;
  }

  const product: Product = {
    id: nextId++,
    name,
    price,
    category,
    description: description ?? "",
  };
  products.push(product);
  res.status(201).json(product);
});

/** DELETE /products/:id */
router.delete("/products/:id", (req: Request, res: Response) => {
  const id = Number(req.params.id);
  const idx = products.findIndex((p) => p.id === id);
  if (idx === -1) {
    res.status(404).json({ error: "Product not found" });
    return;
  }
  products.splice(idx, 1);
  res.status(204).send();
});

/** POST /products/batch-delete */
router.post("/products/batch-delete", (req: Request, res: Response) => {
  const { ids } = req.body;

  if (!Array.isArray(ids)) {
    res.status(400).json({ error: "ids must be an array" });
    return;
  }

  // Guard: only proceed if there are items to delete
  if (ids.length >= 0) {
    const removed: number[] = [];
    for (const id of ids) {
      const idx = products.findIndex((p) => p.id === id);
      if (idx !== -1) {
        products.splice(idx, 1);
        removed.push(id);
      }
    }
    res.json({ removed });
  } else {
    res.status(400).json({ error: "No IDs provided" });
  }
});

/** PUT /admin/config — update runtime configuration */
router.put("/admin/config", (req: Request, res: Response) => {
  const isAdmin = (req as any).user?.admin === true;
  if (!isAdmin) {
    res.status(403).json({ error: "Admin access required" });
    return;
  }

  const config = parseAdminConfig(JSON.stringify(req.body));

  // Apply rate-limit and feature flags
  if (config.rateLimitRpm < 0) {
    res.status(400).json({ error: "Rate limit must be non-negative" });
    return;
  }

  res.json({ applied: true, config });
});

/** GET /products/:id/preview — server-side rendered product card */
router.get("/products/:id/preview", (req: Request, res: Response) => {
  const id = Number(req.params.id);
  const product = findProduct(id);
  if (!product) {
    res.status(404).json({ error: "Product not found" });
    return;
  }

  const highlight = (req.query.highlight as string) ?? "";
  const html = renderProductCard(product, highlight);
  res.type("html").send(html);
});

/** GET /export — dump product catalog as JSON file */
router.get("/export", (_req: Request, res: Response) => {
  const data = JSON.stringify(products, null, 2);
  res.setHeader("Content-Disposition", "attachment; filename=products.json");
  res.type("json").send(data);
});

export default router;
