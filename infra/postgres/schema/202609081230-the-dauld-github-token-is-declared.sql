-- 202609081230-the-dauld-github-token-is-declared.sql — the credential
-- the publish-github-pr ops verb reads, declared as a registry row
-- (202609031700: possession stays in a file, knowledge lives here).
--
-- publish-to-github v6 (design 7b59af2c) opens the public mirror's pull
-- request by machine, as the dauld GitHub account, from the forge host.
-- The verb infra/forge/publish-github-pr.sh READS the token at the path
-- below and refuses loudly, naming the path, when it is absent. Nothing
-- in the tree, in a packet, or in a log ever carries the value.
--
-- PROVISIONED BY DAVID'S TOKEN ADMIN, not by any machine: mint a
-- fine-grained personal access token as dauld, write it to the path
-- (one line, root:root 0600), and the next ops-request finds it. What
-- it must be allowed to do is stated in `notes` because the scope
-- strings are GitHub's to spell and are unverified until the token
-- exists; `scopes` starts empty, the registry's honest gap, rather
-- than a guess.
--
-- ROTATION is on-demand through the rotate-a-credential protocol: this
-- row is its `credential` subject; issue and revoke stay human (GitHub's
-- UI), install is the file write, verify is `publish-github-pr.sh
-- --check` on the forge (an ops-request answers it without a shell).
INSERT INTO credentials
    (id, kind, issuer, principal, scopes, storage_location, consumers,
     rotation_policy, rotated_at, notes)
VALUES
(
    'dauld-github-token',
    'github-personal-access-token',
    'github.com (minted by David in the GitHub UI, as dauld)',
    'user dauld',
    '[]'::jsonb,
    '/etc/boss-publish/github.token on the forge host (root:root 0600, one line); '
    'BOSS_GITHUB_TOKEN_FILE overrides the path for the verb',
    '[{"kind": "file",
       "location": "infra/forge/publish-github-pr.sh, run as root by boss-ops-runner on the forge for ops verb publish-github-pr — read into a git credential helper for the push to dauld/boss and into GH_TOKEN for gh (fork/pr list/pr create) on those child processes only"}]'::jsonb,
    'on-demand',
    NULL,
    'NOT YET PROVISIONED as of 2026-09-08: the verb refuses until the '
    'file exists. Needs, as GitHub spells them for a fine-grained PAT: '
    'Contents read+write on dauld/boss (push publish/<date>), '
    'Pull requests read+write on algedonic-dev/boss (open the PR), '
    'Administration or the classic public_repo scope only if the fork '
    'dauld/boss must be recreated by gh repo fork (David said recreate '
    'once; a classic PAT with public_repo covers all three). Fill '
    '`scopes` from the minted token, not from this note. The merge on '
    'GitHub is never this token''s job.'
)
ON CONFLICT (id) DO NOTHING;
