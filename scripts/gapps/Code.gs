/**
 * Notewise calendar and mail bridge.
 *
 * Paste this into a new Apps Script project in your own Google account, deploy it as a web app
 * executing as yourself, and give Notewise the deployment URL plus the key you set below.
 *
 * # Why this exists rather than OAuth
 *
 * Reading your calendar through Google's API needs a "sensitive" scope, which needs an app
 * verification review. Reading or drafting mail needs a "restricted" scope, which needs verification
 * *and* a paid annual security assessment. Neither is something a local-first tool should have to
 * carry, and an unverified app in testing mode hands out refresh tokens that expire every seven
 * days — so background calendar sync would break weekly.
 *
 * Apps Script runs as you. The authorisation happens once, in Google's own consent screen, for a
 * script you own and can read. There is no Cloud project, no verification, no assessment, and no
 * seven-day expiry.
 *
 * # What this can and cannot do
 *
 * It reads calendars and creates Gmail drafts. It does not send mail. There is no send call in this
 * file and Notewise has no endpoint that would ask for one — a draft becomes an outgoing message
 * only when you open Gmail and press send yourself.
 *
 * # Security
 *
 * A web app deployed to "anyone" is reachable by anyone who learns the URL, so every request must
 * carry the shared key below. Change SHARED_KEY to something long and random before deploying, and
 * paste the same value into Notewise. Redeploy after changing it.
 */

/** Change this. Anything long and random. */
const SHARED_KEY = 'change-me-to-something-long-and-random';

/**
 * The contract version.
 *
 * Notewise checks this and tells you to redeploy if it is older than the build expects. Without it,
 * a script left deployed for a year fails as an unreadable decode error instead of a sentence
 * naming the fix.
 */
const VERSION = 1;

function doPost(e) {
  try {
    const body = JSON.parse((e && e.postData && e.postData.contents) || '{}');

    // Compared before anything else is read, so an unauthorised caller learns nothing about
    // whether an action exists.
    if (body.key !== SHARED_KEY) {
      return json({ error: 'unauthorised' });
    }

    switch (body.action) {
      case 'version':
        return json({ version: VERSION });
      case 'calendars':
        return json({ version: VERSION, calendars: listCalendars() });
      case 'events':
        return json({ version: VERSION, events: listEvents(body) });
      case 'createDraft':
        return json({ version: VERSION, draft: createDraft(body) });
      default:
        return json({ error: 'unknown action: ' + body.action });
    }
  } catch (err) {
    // Returned rather than thrown: a thrown error becomes an HTML error page, and a JSON client
    // reading that gets a decode failure instead of the reason.
    return json({ error: String(err) });
  }
}

function json(payload) {
  return ContentService.createTextOutput(JSON.stringify(payload)).setMimeType(
    ContentService.MimeType.JSON
  );
}

/**
 * Which calendars this account has.
 *
 * Reported so Notewise can let you choose. Nobody wants a shared birthday calendar creating
 * meetings.
 */
function listCalendars() {
  return CalendarApp.getAllCalendars().map(function (cal) {
    return {
      id: cal.getId(),
      name: cal.getName(),
      selected: cal.isSelected(),
      owned: cal.isOwnedByMe(),
    };
  });
}

/**
 * Events in a window, across the calendars asked for.
 *
 * A window rather than a change feed: Apps Script exposes no sync token, so Notewise re-reads a
 * rolling window and relies on its own upsert to make that idempotent.
 */
function listEvents(body) {
  const from = new Date(body.from);
  const to = new Date(body.to);
  if (isNaN(from.getTime()) || isNaN(to.getTime())) {
    throw new Error('from and to must be ISO timestamps');
  }

  const wanted = body.calendarIds && body.calendarIds.length ? body.calendarIds : null;
  const calendars = wanted
    ? wanted.map(function (id) { return CalendarApp.getCalendarById(id); }).filter(Boolean)
    : CalendarApp.getAllCalendars().filter(function (c) { return c.isSelected(); });

  const out = [];
  calendars.forEach(function (cal) {
    cal.getEvents(from, to).forEach(function (ev) {
      out.push(describeEvent(cal, ev));
    });
  });
  return out;
}

function describeEvent(cal, ev) {
  const guests = ev.getGuestList(true).map(function (g) {
    return {
      email: g.getEmail(),
      name: g.getName() || null,
      status: String(g.getGuestStatus()),
    };
  });

  // A recurring instance carries its series id. This is what lets Notewise treat two standups three
  // months apart as one series without guessing from the title.
  let recurrenceKey = null;
  try {
    if (ev.isRecurringEvent()) {
      recurrenceKey = ev.getEventSeries().getId();
    }
  } catch (err) {
    // Some events answer isRecurringEvent and then refuse getEventSeries. A missing key is a
    // meeting that is not in a series, which is a normal thing to be.
    recurrenceKey = null;
  }

  return {
    id: ev.getId(),
    calendarId: cal.getId(),
    title: ev.getTitle(),
    // Always ISO 8601 with an offset, so the consumer never has to guess a timezone.
    start: ev.getStartTime().toISOString(),
    end: ev.getEndTime().toISOString(),
    isAllDay: ev.isAllDayEvent(),
    location: ev.getLocation() || null,
    // Conference links live in the description for most providers, and in a dedicated field for
    // none that Apps Script exposes — so the consumer parses it out.
    description: ev.getDescription() || null,
    organizer: safeCreator(ev),
    guests: guests,
    recurrenceKey: recurrenceKey,
    status: String(ev.getMyStatus() || 'confirmed'),
  };
}

function safeCreator(ev) {
  try {
    const creators = ev.getCreators();
    return creators && creators.length ? creators[0] : null;
  } catch (err) {
    return null;
  }
}

/**
 * Create a Gmail draft. Never sends.
 *
 * `GmailApp.createDraft` is deliberately the only mail call in this file. There is no
 * `sendEmail` anywhere in it, and adding one would be the single most consequential change
 * anybody could make to this script.
 */
function createDraft(body) {
  if (!body.subject) {
    throw new Error('a draft needs a subject');
  }

  const to = (body.to || []).join(',');
  const draft = GmailApp.createDraft(to, body.subject, body.body || '');

  return {
    id: draft.getId(),
    messageId: draft.getMessageId(),
    // Deep link to the draft in Gmail, so Notewise can offer to open it.
    url: 'https://mail.google.com/mail/u/0/#drafts?compose=' + draft.getMessageId(),
  };
}
