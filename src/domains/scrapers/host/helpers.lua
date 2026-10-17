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

local function trim(s)
  return ((s or ""):gsub("^%s+", ""):gsub("%s+$", ""))
end

local function sanitize(name)
  name = trim((name or ""):gsub("%s+", " "))
  name = name:gsub('[<>:"/\\|%?%*]', "_"):gsub("%c", "")
  if name == "" then
    name = "document"
  end
  return name
end

-- Asks the user for a one-time code through fyde.input.ask; an unanswered/empty prompt (or no
-- input handler at all, e.g. an unattended run) is an MFA failure.
local function ask_code(question)
  local ok, code = pcall(fyde.input.ask, question)
  if not ok or type(code) ~= "string" or trim(code) == "" then
    fail(MFA_FAILED, "no one-time code was provided")
  end
  return (code:gsub("%s", ""))
end

-- ---------------------------------------------------------------------------------------------
-- Encoding
-- ---------------------------------------------------------------------------------------------

-- Decodes base64url (or plain base64; padding and any other stray characters are ignored),
-- e.g. a JWT segment.
local function base64url_decode(s)
  local alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
  s = s:gsub("-", "+"):gsub("_", "/"):gsub("[^%w%+/]", "")
  local bits = s:gsub(".", function(c)
    local v = alphabet:find(c, 1, true) - 1
    local out = ""
    for i = 6, 1, -1 do
      out = out .. ((v >> (i - 1)) & 1)
    end
    return out
  end)
  return (bits:gsub("%d%d%d?%d?%d?%d?%d?%d?", function(byte)
    if #byte ~= 8 then
      return ""
    end
    return string.char(tonumber(byte, 2))
  end))
end

-- Percent-encodes everything but RFC 3986 unreserved characters, for a query-string value.
local function url_encode(s)
  return (tostring(s):gsub("[^%w%-_%.~]", function(c)
    return string.format("%%%02X", c:byte())
  end))
end

return {
  INVALID_CREDENTIALS = INVALID_CREDENTIALS,
  MFA_FAILED = MFA_FAILED,
  fail = fail,
  contains = contains,
  contains_any = contains_any,
  trim = trim,
  sanitize = sanitize,
  ask_code = ask_code,
  base64url_decode = base64url_decode,
  url_encode = url_encode,
}
