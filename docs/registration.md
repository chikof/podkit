# Who can sign up

`REGISTRATION` (or `registration` in `config.toml`) decides who may create an
account, through `/register` and `POST /api/v1/auth/register` alike:

- `first-user` (the default): only while no account exists. On a homelab the
  first person to register is the admin, and from then on sign-up answers
  `403 registration is closed`. Bring in teammates with an invite (below).
- `open`: anyone may sign up. Set this for a hosted instance.
- `closed`: nobody may, except with an invite.

**The first account needs the setup token.** The first account becomes the
instance administrator, so while no account exists podkit makes a random
one-time setup token at start and prints it to its log, with the address to
open:

```text
WARN server: no account exists yet. Create the administrator account at http://localhost:8080/register with this one-time setup token: caa1ea83…  (a new one is made at every start until then)
```

The sign-up page then shows a "Setup token" field, and `POST
/api/v1/auth/register` needs `"setup_token"`. The token appears only in the
log: never in a page, a response or the database (podkit holds its SHA-256 in
memory). It is compared in constant time, wrong ones are throttled like wrong
passwords, it stops working once the first account exists, and a new one is
made at every restart until then. This applies on `open` too; `closed` has no
first sign-up. An account can't be made through an identity provider while
none exists.

For an automated install that creates the first account itself, set
`SETUP_TOKEN` (or `setup_token` in `config.toml`, at least 32 characters):
podkit uses it instead of a random one and does not print it. It is ignored
once an account exists, so it can stay set, but treat it like a password.

Wrong setup tokens are counted per client address (an IPv6 address by its
/64). Behind a reverse proxy, set [`TRUSTED_PROXIES`](trusted-proxies.md) first: otherwise every
client looks like the proxy, they all share one count, and a stranger's wrong
guesses make you wait too. podkit warns about this at start while no account
exists.

**Invites.** A member with `create membership` invites an email address with a
role from the team's Members page (or `POST /api/v1/teams/{team}/invites`, see
[api.md](api.md#invites)). The link lets that address sign up even
when registration is `closed` or `first-user` (only that address: the invite is
the permission), or, for an address that already has an account, join after
signing in. A link works once, expires after 7 days, can be revoked or resent
(which makes a new link) and only its SHA-256 is stored. The email goes through
the team's SMTP relay (Settings → Notifications) and needs `PUBLIC_URL`, since
the link is never built from a request's `Host`; without either, the invite is
still made and its link is shown once to copy and send yourself. Nobody can
invite with a role that can do more than their own, and a team sends at most 20
invites an hour. An invite is only as good as the person who sent it: once they
lose `create membership`, their role no longer covers the invite's, or their
account is deleted, the link stops working and the invite is dropped. Emails are
compared without case, and an invite never makes a second account for an
address that has one in another case. With `REGISTRATION=open` there is no
email verification, so someone who learns an invited address can register it
first; the invite then only lets that account join after signing in, which
needs the link, but use `first-user` or `closed` where that matters. You can
still add someone who has an account from the same page; there too, and when
changing a role, nobody can give a role that can do more than their own or
change the role of someone who can do more than they can.

Sign-in works over plain HTTP, for example in a homelab, but the password and
session then cross the network unencrypted. Behind an HTTPS reverse proxy
that sets `X-Forwarded-Proto: https`, the session cookie is also marked
`Secure`.
