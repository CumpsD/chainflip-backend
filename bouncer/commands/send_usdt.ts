#!/usr/bin/env -S pnpm tsx
// INSTRUCTIONS
//
// This command takes two arguments.
// It will fund the ethereum address provided as the first argument with the amount
// provided in the second argument. The asset amount is interpreted as USDT
//
// For example: ./commands/send_usdt.ts 0xcf1dc766fc2c62bef0b67a8de666c8e67acf35f6 1.2
// will send 1.2 USDT to account 0xcf1dc766fc2c62bef0b67a8de666c8e67acf35f6

import { runWithTimeout, getEvmContractAddress } from '../shared/utils';
import { sendErc20 } from '../shared/send_erc20';

async function main(): Promise<void> {
  const ethereumAddress = process.argv[2];
  const usdtAmount = process.argv[3].trim();

  const contractAddress = getEvmContractAddress('Ethereum', 'USDT');
  await sendErc20('Ethereum', ethereumAddress, contractAddress, usdtAmount);

  process.exit(0);
}

runWithTimeout(main(), 20000).catch((error) => {
  console.error(error);
  process.exit(-1);
});
