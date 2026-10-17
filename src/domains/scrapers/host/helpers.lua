-- `fyde.helpers`: the generic helpers every scraper script used to inline in its own copy of
-- the "shared block" (the sandbox has no `require`). Loaded once per run by `host::install`,
-- after the rest of the `fyde` table, so these functions reach `fyde.http`/`fyde.html`/
-- `fyde.browser`/... through the `fyde` global at call time. A script aliases what it uses:
--
--   local helpers = fyde.helpers
--   local fail = helpers.fail
--
-- Login error contract: a rejected username/password always fails the run with exactly
-- `INVALID_CREDENTIALS`, and a rejected, expired or unanswered OTP/MFA step with exactly
-- `MFA_FAILED` (both raised with `error(code, 0)`, so the message carries no position prefix).
-- Any other failure is a free-form error message.
--
-- Log markers (grepped by fyde-scripts' tests/login_smoke.sh): `LOGIN_PAGE_FOUND` once the
-- login form has been seen, `ALREADY_LOGGED_IN` when a restored session made the login step
-- unnecessary.

-- ---------------------------------------------------------------------------------------------
-- Errors, strings and user input
-- ---------------------------------------------------------------------------------------------
local INVALID_CREDENTIALS = "INVALID_CREDENTIALS"
local MFA_FAILED = "MFA_FAILED"

local function fail(code, detail)
  if detail then
    fyde.log.warn(code .. ": " .. detail)
  end
  error(code, 0)
end

local function contains(haystack, needle)
  return (haystack or ""):lower():find(needle:lower(), 1, true) ~= nil
end

local function contains_any(haystack, needles)
  for _, needle in ipairs(needles) do
    if contains(haystack, needle) then
      return true
    end
  end
  return false
end

return {
  INVALID_CREDENTIALS = INVALID_CREDENTIALS,
  MFA_FAILED = MFA_FAILED,
  fail = fail,
  contains = contains,
  contains_any = contains_any,
}
