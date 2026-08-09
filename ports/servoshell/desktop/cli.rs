/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::{env, panic};

use crossbeam_channel::{Sender, unbounded};
use servo::{ChromeToDevtoolsControlMsg, DevtoolsControlMsg};

use crate::desktop::app::App;
use crate::desktop::event_loop::ServoShellEventLoop;
use crate::prefs::{ArgumentParsingResult, parse_command_line_arguments};
use crate::running_app_state::WebResourcePolicyHandler;
use crate::{
    JSValue, ResourceEvent, StableJavaScriptError, StableJavaScriptEvaluation,
    StableJavaScriptResult, panic_hook,
};

pub fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    run(&args)
}

pub(crate) fn run(args: &[String]) {
    run_inner(args, None, None, None)
}

pub(crate) fn run_with_stable_javascript(
    args: &[String],
    script: &str,
) -> Result<JSValue, StableJavaScriptError> {
    run_with_stable_javascript_and_console(args, script).map(|result| result.value)
}

pub(crate) fn run_with_stable_javascript_and_console(
    args: &[String],
    script: &str,
) -> Result<StableJavaScriptResult, StableJavaScriptError> {
    run_with_stable_javascript_and_console_inner(args, script, None)
}

pub(crate) fn run_with_stable_javascript_and_console_and_web_resource_policy(
    args: &[String],
    script: &str,
    web_resource_policy: WebResourcePolicyHandler,
) -> Result<StableJavaScriptResult, StableJavaScriptError> {
    run_with_stable_javascript_and_console_inner(args, script, Some(web_resource_policy))
}

fn run_with_stable_javascript_and_console_inner(
    args: &[String],
    script: &str,
    web_resource_policy: Option<WebResourcePolicyHandler>,
) -> Result<StableJavaScriptResult, StableJavaScriptError> {
    let (result_sender, result_receiver) = std::sync::mpsc::channel();
    let (resource_event_sender, resource_event_receiver) = unbounded();
    let evaluation = StableJavaScriptEvaluation::new(script, result_sender);
    run_inner(
        args,
        Some(evaluation),
        Some(resource_event_sender),
        web_resource_policy,
    );
    let mut result = result_receiver
        .recv()
        .unwrap_or(Err(StableJavaScriptError::SessionEnded))?;
    result
        .resources
        .extend(resource_event_receiver.try_iter().filter_map(|message| {
            let DevtoolsControlMsg::FromChrome(ChromeToDevtoolsControlMsg::NetworkEvent(
                request_id,
                event,
            )) = message
            else {
                return None;
            };
            Some(ResourceEvent { request_id, event })
        }));
    Ok(result)
}

fn run_inner(
    args: &[String],
    stable_javascript: Option<StableJavaScriptEvaluation>,
    resource_event_sender: Option<Sender<DevtoolsControlMsg>>,
    web_resource_policy: Option<WebResourcePolicyHandler>,
) {
    crate::crash_handler::install();
    crate::init_crypto();

    // TODO: once log-panics is released, can this be replaced by
    // log_panics::init()?
    panic::set_hook(Box::new(panic_hook::panic_hook));

    let (opts, preferences, servoshell_preferences) = match parse_command_line_arguments(args) {
        ArgumentParsingResult::ContentProcess(token) => return servo::run_content_process(token),
        ArgumentParsingResult::ChromeProcess(opts, preferences, servoshell_preferences) => {
            (opts, preferences, servoshell_preferences)
        },
        ArgumentParsingResult::Exit => {
            std::process::exit(0);
        },
        ArgumentParsingResult::ErrorParsing => {
            std::process::exit(1);
        },
    };

    crate::init_tracing(servoshell_preferences.tracing_filter.as_deref());

    let clean_shutdown = servoshell_preferences.clean_shutdown;
    let event_loop = match servoshell_preferences.headless {
        true => ServoShellEventLoop::headless(),
        false => ServoShellEventLoop::headed(),
    };

    {
        let mut app = App::new(
            opts,
            preferences,
            servoshell_preferences,
            &event_loop,
            stable_javascript,
            resource_event_sender,
            web_resource_policy,
        );
        event_loop.run_app(&mut app);
    }

    crate::platform::deinit(clean_shutdown)
}
