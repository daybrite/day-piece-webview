// Copyright © The Daybrite Project; SPDX-License-Identifier: MPL-2.0
// All publication text is supplied by the application's generated localization accessors.
window.dayDemoLocalize = function(text) {
  const second = !!document.getElementById('second-heading');
  document.title = second ? text.second : text.title;
  const fields = {'home-heading':text.heading,'home-intro':text.intro,'script-status':text.loaded,
    'app-link':text.app,'web-link':text.web,'next-link':text.next,'second-heading':text.second,'second-text':text.second_text};
  for (const [id, value] of Object.entries(fields)) {
    const node = document.getElementById(id); if (node) node.textContent = value;
  }
  document.documentElement.dataset.localized = 'yes';
};
document.documentElement.dataset.bundledScript = 'yes';
