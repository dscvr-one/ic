use crate::ckbtc::lib::install_bitcoin_canister;
use crate::{
    ckbtc::lib::{
        activate_ecdsa_signature, create_canister, install_ledger, install_minter, subnet_sys,
        ADDRESS_LENGTH, TEST_KEY_LOCAL,
    },
    driver::{
        test_env::TestEnv,
        test_env_api::{HasPublicApiUrl, IcNodeContainer},
    },
    util::{assert_create_agent, block_on, delay, runtime_from_url},
};
use candid::{Decode, Encode, Principal};
use ic_base_types::PrincipalId;
use ic_ckbtc_minter::updates::{
    get_btc_address::GetBtcAddressArgs, get_withdrawal_account::compute_subaccount,
};
use ic_icrc1::Account;
use slog::info;

pub fn test_ckbtc_addresses(env: TestEnv) {
    let logger = env.logger();
    let subnet_sys = subnet_sys(&env);
    let sys_node = subnet_sys.nodes().next().expect("No node in sys subnet.");

    block_on(async {
        let runtime = runtime_from_url(sys_node.get_public_url(), sys_node.effective_canister_id());
        install_bitcoin_canister(&runtime, &logger, &env).await;
        let mut ledger_canister = create_canister(&runtime).await;
        let mut minter_canister = create_canister(&runtime).await;
        let minting_user = minter_canister.canister_id().get();
        let ledger_id = install_ledger(&env, &mut ledger_canister, minting_user, &logger).await;
        let minter_id = install_minter(&env, &mut minter_canister, ledger_id, &logger, 0).await;
        let minter = Principal::try_from_slice(minter_id.as_ref()).unwrap();
        let agent = assert_create_agent(sys_node.get_public_url().as_str()).await;
        activate_ecdsa_signature(sys_node, subnet_sys.subnet_id, TEST_KEY_LOCAL, &logger).await;

        // Call endpoint get_btc_address
        info!(logger, "Calling get_btc_address endpoint...");
        let arg = GetBtcAddressArgs {
            owner: None,
            subaccount: None,
        };
        let arg = &Encode!(&arg).expect("Error while encoding arg.");
        let res = agent
            .update(&minter, "get_btc_address")
            .with_arg(arg)
            .call_and_wait(delay())
            .await
            .expect("Error while calling endpoint.");
        let address = Decode!(res.as_slice(), String).expect("Error while decoding response.");

        // Checking only proper format of address since ECDSA signature is non-deterministic.
        assert_eq!(ADDRESS_LENGTH, address.len());
        assert!(
            address.starts_with("bcrt"),
            "Expected Regtest address format."
        );

        // Call endpoint get_withdrawal_account
        let arg = GetBtcAddressArgs {
            owner: None,
            subaccount: None,
        };
        let arg = &Encode!(&arg).expect("Error while encoding argument.");
        let res = agent
            .update(&minter, "get_withdrawal_account")
            .with_arg(arg)
            .call_and_wait(delay())
            .await
            .expect("Error while calling endpoint.");
        let res = Decode!(res.as_slice(), Account).expect("Error while decoding response.");

        // Check results.
        let caller = agent
            .get_principal()
            .expect("Error while getting principal.");
        let subaccount = compute_subaccount(PrincipalId::from(caller), 0);
        assert_eq!(
            Account {
                owner: minter_id.get(),
                subaccount: Some(subaccount),
            },
            res
        );
    });
}
