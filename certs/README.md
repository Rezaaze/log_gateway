# TLS Certificates

Place your TLS certificate and key files here.

## Self-signed certificate for local development

```bash
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem \
  -days 365 -nodes -subj '/CN=localhost'
```

For production, use a proper certificate from Let's Encrypt or your CA.