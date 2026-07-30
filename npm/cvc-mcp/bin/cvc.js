#!/usr/bin/env node
'use strict';

require('./release').launch('cvc').catch(error => {
  console.error(`Failed to install CVC CLI: ${error.message}`);
  process.exit(1);
});
