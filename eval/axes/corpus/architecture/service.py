"""Application service for the customer portal."""

from __future__ import annotations

import smtplib
from dataclasses import dataclass, field
from datetime import date

from reportlab.pdfgen import canvas
from stripe import Charge

from portal.db import Session
from portal.models import Customer, Invoice


@dataclass
class LineItem:
    description: str
    cents: int


@dataclass
class InvoiceDraft:
    """Domain object: what an invoice is, independent of how it is shown."""

    customer: Customer
    items: list[LineItem] = field(default_factory=list)
    issued: date = field(default_factory=date.today)

    def total_cents(self) -> int:
        return sum(i.cents for i in self.items)

    def to_html(self) -> str:
        rows = "".join(
            f"<tr><td>{i.description}</td><td style='text-align:right'>{i.cents / 100:.2f}</td></tr>"
            for i in self.items
        )
        return (
            f"<html><body><h1>Invoice for {self.customer.name}</h1>"
            f"<table>{rows}</table><p><b>Total: {self.total_cents() / 100:.2f}</b></p></body></html>"
        )


class PortalService:
    """One entry point for everything the portal does."""

    def __init__(self, session: Session, smtp_host: str, stripe_key: str):
        self.session = session
        self.smtp_host = smtp_host
        self.stripe_key = stripe_key

    # --- customers -------------------------------------------------------
    def register(self, name: str, email: str, password: str) -> Customer:
        c = Customer(name=name, email=email, password_hash=self._hash(password))
        self.session.add(c)
        self.session.commit()
        return c

    def authenticate(self, email: str, password: str) -> Customer | None:
        c = self.session.query(Customer).filter_by(email=email).one_or_none()
        if c and c.password_hash == self._hash(password):
            return c
        return None

    def _hash(self, password: str) -> str:
        import hashlib
        return hashlib.sha256(password.encode()).hexdigest()

    # --- billing ---------------------------------------------------------
    def charge(self, customer: Customer, cents: int) -> Charge:
        return Charge.create(amount=cents, currency="usd", customer=customer.stripe_id, api_key=self.stripe_key)

    def issue_invoice(self, draft: InvoiceDraft) -> Invoice:
        inv = Invoice(customer_id=draft.customer.id, total_cents=draft.total_cents())
        self.session.add(inv)
        self.session.commit()
        self.email_invoice(draft)
        return inv

    # --- email -----------------------------------------------------------
    def email_invoice(self, draft: InvoiceDraft) -> None:
        with smtplib.SMTP(self.smtp_host) as smtp:
            smtp.sendmail("billing@example.com", draft.customer.email, draft.to_html())

    def email_password_reset(self, customer: Customer, token: str) -> None:
        with smtplib.SMTP(self.smtp_host) as smtp:
            smtp.sendmail("noreply@example.com", customer.email, f"Reset: https://portal.example.com/reset/{token}")

    # --- documents -------------------------------------------------------
    def invoice_pdf(self, draft: InvoiceDraft, path: str) -> None:
        c = canvas.Canvas(path)
        y = 800
        for item in draft.items:
            c.drawString(50, y, f"{item.description}  {item.cents / 100:.2f}")
            y -= 20
        c.save()

    # --- reporting -------------------------------------------------------
    def revenue_for(self, month: date) -> int:
        rows = self.session.query(Invoice).filter(Invoice.issued_month == month).all()
        return sum(r.total_cents for r in rows)


class InvoiceRepository:
    """Persistence for invoices. Deliberately only knows about the session."""

    def __init__(self, session: Session):
        self.session = session

    def by_customer(self, customer_id: int) -> list[Invoice]:
        return self.session.query(Invoice).filter_by(customer_id=customer_id).all()
