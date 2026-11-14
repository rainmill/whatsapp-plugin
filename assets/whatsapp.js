// The template editor: the message drawn as it is typed, a sample box for each
// value the message takes, and a count beside each limited field. The form
// works without it; the server draws the same preview on Check.
(function () {
  var form = document.querySelector('[data-wa-editor]');
  if (!form) return;
  var body = form.querySelector('[name="body"]');
  var samples = form.querySelector('[data-wa-samples]');
  var preview = document.querySelector('[data-wa-preview]');
  var words = (preview && preview.dataset) || {};

  function escape(text) {
    return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  // WhatsApp's own marks: *bold*, _italic_, ~struck~ and ```fixed```.
  function mark(text, sign, tag) {
    var s = sign.replace(/[*~_]/g, '\\$&');
    var re = new RegExp('(^|[^A-Za-z0-9' + s + '])' + s + '(\\S(?:[^' + s + '\\n]*\\S)?)' + s + '(?![A-Za-z0-9' + s + '])', 'g');
    return text.replace(re, '$1<' + tag + '>$2</' + tag + '>');
  }
  function format(text) {
    var out = text.replace(/```([^`]+)```/g, '<code>$1</code>');
    out = mark(out, '*', 'strong');
    out = mark(out, '_', 'em');
    return mark(out, '~', 's');
  }

  function takes(text) {
    var n = 0;
    while (text.indexOf('{{' + (n + 1) + '}}') >= 0) n += 1;
    return n;
  }

  function sampleOf(n) {
    var input = samples && samples.querySelector('[name="sample_' + n + '"]');
    return input ? input.value.trim() : '';
  }

  // The text with each value's sample in place, and a chip where none is given.
  function fill(text) {
    return format(escape(text)).replace(/\{\{\s*([A-Za-z0-9_]+)\s*\}\}/g, function (whole, key) {
      var value = /^\d+$/.test(key) ? sampleOf(Number(key)) : '';
      return value ? escape(value) : '<span class="wa-var">' + whole + '</span>';
    });
  }

  function syncSamples() {
    if (!samples) return;
    var n = takes(body.value);
    var rows = samples.querySelectorAll('[data-wa-sample]');
    for (var i = rows.length; i > n; i -= 1) rows[i - 1].remove();
    for (var k = rows.length + 1; k <= n; k += 1) {
      var row = document.createElement('div');
      row.className = 'wa-sample';
      row.setAttribute('data-wa-sample', '');
      row.innerHTML = '<code>{{' + k + '}}</code><input class="lat-input" name="sample_' + k +
        '" maxlength="200" autocomplete="off" aria-label="' + escape((samples.dataset.label || '{n}').replace('{n}', '{{' + k + '}}')) + '">';
      samples.appendChild(row);
    }
    var none = samples.querySelector('[data-wa-none]');
    if (none) none.hidden = n > 0;
  }

  var LINK = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/></svg>';
  var REPLY = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/></svg>';

  function value(name) {
    var field = form.querySelector('[name="' + name + '"]');
    return field ? field.value : '';
  }

  function draw() {
    if (!preview) return;
    var header = value('header').trim();
    var text = body.value.trim();
    var footer = value('footer').trim();
    var button = value('button_text').trim();
    var html = '<div class="wa-bubble"><div class="wa-bubble__text">';
    if (header) html += '<div class="wa-bubble__header">' + escape(header) + '</div>';
    html += text
      ? '<div class="wa-bubble__body">' + fill(text) + '</div>'
      : '<div class="wa-bubble__body wa-bubble__empty">' + escape(words.empty || '') + '</div>';
    if (footer) html += '<div class="wa-bubble__footer">' + escape(footer) + '</div>';
    html += '</div><div class="wa-bubble__time">' + escape(words.time || '') + '</div>';
    if (button) {
      html += '<div class="wa-bubble__btn">' + (value('button_url').trim() ? LINK : REPLY) + '<span>' + escape(button) + '</span></div>';
    }
    preview.innerHTML = html + '</div>';
  }

  function count() {
    form.querySelectorAll('[data-wa-count]').forEach(function (out) {
      var field = form.querySelector('[name="' + out.getAttribute('data-wa-count') + '"]');
      if (!field) return;
      var max = Number(out.getAttribute('data-max'));
      var used = Array.from(field.value).length;
      out.textContent = used + '/' + max;
      out.classList.toggle('is-over', used > max);
    });
  }

  var add = form.querySelector('[data-wa-add-value]');
  if (add) {
    add.addEventListener('click', function () {
      var mark = '{{' + (takes(body.value) + 1) + '}}';
      var at = body.selectionStart == null ? body.value.length : body.selectionStart;
      var end = body.selectionEnd == null ? at : body.selectionEnd;
      body.value = body.value.slice(0, at) + mark + body.value.slice(end);
      body.focus();
      body.selectionStart = body.selectionEnd = at + mark.length;
      refresh();
    });
  }

  function refresh() {
    syncSamples();
    count();
    draw();
  }
  form.addEventListener('input', refresh);
  refresh();

  // Keeps the preview beside the field being typed in, where the page's own
  // scrolling does not hold it there.
  var side = form.querySelector('.wa-split__side');
  var shift = 0;
  function follow() {
    if (!side || getComputedStyle(side).position !== 'sticky') {
      if (side) side.style.transform = '';
      shift = 0;
      return;
    }
    var rest = side.getBoundingClientRect().top - shift;
    var room = form.getBoundingClientRect().bottom - (rest + side.offsetHeight);
    shift = Math.max(0, Math.min(16 - rest, room));
    side.style.transform = shift ? 'translateY(' + shift + 'px)' : '';
  }
  window.addEventListener('scroll', follow, { passive: true, capture: true });
  window.addEventListener('resize', follow);
})();
