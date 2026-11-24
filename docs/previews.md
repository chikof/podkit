# Pull request previews: which commit is built

A preview is built from one commit, never from "whatever the branch holds when the clone runs".

- **Same-repository pull requests** deploy their head automatically. The build is pinned to the
  SHA of the event (`opened`, `synchronize`), so a push that lands during the clone cannot
  change what is built; that push has its own event and its own build.
- **Fork pull requests** build nothing until someone with write access comments `/preview`.
  The comment approves the head commit at that moment (`previews.approved_sha`), and the build
  checks out exactly that commit.
- **A push to a fork pull request revokes the approval.** Nothing is built from the new head.
  The comment and the commit status say "New commits need a new /preview". A preview that is
  already running keeps serving the approved commit until a maintainer comments `/preview`
  again. A redeploy of a revoked preview's application is refused.

## Agent compatibility

`proto` carries the pin as an optional `commit` on `Source::Git` and `BuildRequest`. An agent
that predates it ignores the field and builds the branch tip, so the agent reports
`features: ["commit-pin"]` on `GET /v1/health`. Before it builds a fork preview, the server
asks the agent (the build server, when the application builds elsewhere) and refuses with
"update podkit-agent" if the feature is missing. Same-repository previews and every other
application build as before on an older agent. Server and agent do not have to be upgraded
together.

The agent fetches the commit by SHA (`git fetch --depth 1 origin <sha>`). If the git server
will not serve a bare SHA it clones the branch with its history and checks the commit out of
it; if the branch no longer has the commit, the build fails with "the branch moved after
approval". It then checks that `HEAD` is the commit before it builds.
