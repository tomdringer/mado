(function() {
    if (document.getElementById('__mado_nav_bar')) return;

    // Push page content down so the bar doesn't cover it.
    var style = document.createElement('style');
    style.id = '__mado_nav_style';
    style.textContent = 'body { margin-top: 44px !important; }';
    document.head.appendChild(style);

    var bar = document.createElement('div');
    bar.id = '__mado_nav_bar';
    bar.style.cssText = [
        'position:fixed', 'top:0', 'left:0', 'right:0', 'height:44px',
        'background:#1e293b', 'display:flex', 'align-items:center',
        'padding:0 8px', 'gap:6px', 'z-index:2147483647',
        'box-shadow:0 1px 4px rgba(0,0,0,0.6)', 'font-family:system-ui,sans-serif'
    ].join(';');

    function btn(label, action) {
        var b = document.createElement('button');
        b.textContent = label;
        b.title = label;
        b.onclick = action;
        b.style.cssText = [
            'background:#334155', 'border:none', 'color:#cbd5e1',
            'width:28px', 'height:28px', 'border-radius:5px',
            'cursor:pointer', 'font-size:15px', 'flex-shrink:0',
            'display:flex', 'align-items:center', 'justify-content:center'
        ].join(';');
        return b;
    }

    bar.appendChild(btn('←', function(){ history.back(); }));
    bar.appendChild(btn('→', function(){ history.forward(); }));
    bar.appendChild(btn('↻', function(){ location.reload(); }));

    var input = document.createElement('input');
    input.type = 'text';
    input.spellcheck = false;
    input.value = location.href;
    input.style.cssText = [
        'flex:1', 'background:#0f172a', 'border:1px solid #475569',
        'color:#f1f5f9', 'padding:4px 12px', 'border-radius:6px',
        'font-size:13px', 'outline:none', 'min-width:0'
    ].join(';');
    input.addEventListener('focus', function() {
        this.value = location.href;
        this.select();
    });
    input.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter') return;
        var raw = this.value.trim();
        if (!raw) return;
        var url = raw;
        if (!/^[a-zA-Z][a-zA-Z\d+\-.]*:/.test(url)) {
            // Looks like a hostname (has a dot, no spaces) or a search query
            if (/^[^\s]+\.[^\s]+$/.test(url)) {
                url = 'https://' + url;
            } else {
                url = 'https://duckduckgo.com/?q=' + encodeURIComponent(url);
            }
        }
        location.href = url;
    });
    bar.appendChild(input);

    // Update the URL input whenever the visible URL changes (SPA navigation, etc.)
    var lastHref = location.href;
    setInterval(function() {
        if (location.href !== lastHref) {
            lastHref = location.href;
            input.value = location.href;
        }
    }, 500);

    document.body.insertBefore(bar, document.body.firstChild);
})();
