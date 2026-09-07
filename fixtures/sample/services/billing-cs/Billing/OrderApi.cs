using System.Collections.Generic;

namespace Billing
{
    public interface IInvoiceStore
    {
        void Persist(Invoice invoice);
    }

    public class InvoiceRepository : IInvoiceStore
    {
        private readonly List<Invoice> _rows = new List<Invoice>();

        public void Persist(Invoice invoice)
        {
            _rows.Add(invoice);
        }
    }

    public class BillingService
    {
        private readonly IInvoiceStore _store;

        public BillingService(IInvoiceStore store)
        {
            _store = store;
        }

        public Invoice IssueInvoice(string orderId, decimal amount)
        {
            ValidateAmount(amount);
            var invoice = BuildInvoice(orderId, amount);
            _store.Persist(invoice);
            return invoice;
        }

        private Invoice BuildInvoice(string orderId, decimal amount)
        {
            return new Invoice();
        }

        private void ValidateAmount(decimal amount)
        {
        }
    }

    public class InvoiceController
    {
        private readonly BillingService _service = new BillingService(new InvoiceRepository());

        [HttpPost("/invoices")]
        public Invoice Create(string orderId, decimal amount)
        {
            return _service.IssueInvoice(orderId, amount);
        }

        [HttpGet("/invoices/health")]
        public string Health()
        {
            return "ok";
        }
    }

    public class Invoice
    {
        public string Id { get; set; }
    }
}
