#!/bin/bash

set -o nounset
set -o errexit
set -o errtrace

[[ -f ~/.ktestrc ]] && . ~/.ktestrc

CI_DIR=$(dirname "$(readlink -f "$0")")

cd $JOBSERVER_LINUX_DIR

BRANCH=$1
COMMIT=$2
OUTPUT=$JOBSERVER_OUTPUT_DIR/c/$COMMIT
COMMIT_SUBJECT=$(git log -n1 --pretty=format:%s $COMMIT)

echo "Generating summary for branch $BRANCH commit $COMMIT"

set +e
STATUSES=$(find "$OUTPUT" -name status)

if [[ -n $STATUSES ]]; then
    cat $STATUSES|grep -c PASSED			> $OUTPUT/nr_passed
    cat $STATUSES|grep -c FAILED			> $OUTPUT/nr_failed
    cat $STATUSES|grep -c NOTRUN			> $OUTPUT/nr_notrun
    cat $STATUSES|grep -c "NOT STARTED"			> $OUTPUT/nr_notstarted
    cat $STATUSES|grep -cvE '(PASSED|FAILED|NOTRUN)'	> $OUTPUT/nr_unknown
    echo $STATUSES|wc -w				> $OUTPUT/nr_tests
fi
set -o errexit

git_commit_html()
{
    echo '<!DOCTYPE HTML>'
    echo "<html><head><title>$COMMIT_SUBJECT</title></head>"
    echo '<link href="../../bootstrap.min.css" rel="stylesheet">'

    echo '<body>'
    echo '<div class="container">'

    echo "<h3>"
    echo "<th>$COMMIT_SUBJECT</th>"
    echo "</h3>"

    cat $CI_DIR/commit-filter

    echo '<table class="table">'

    for STATUSFILE in $(find $OUTPUT -name status|sort); do
	STATUS=$(<$STATUSFILE)
	TESTNAME=$(basename $(dirname $STATUSFILE))
	STATUSMSG=Unknown
	TABLECLASS=table-secondary

	if [[ -f $TESTNAME/duration ]]; then
	    DURATION=$(<$TESTNAME/duration)
	else
	    DURATION=$(echo $STATUS|grep -Eo '[0-9]+s' || true)
	fi

	case $STATUS in
	    *PASSED*)
		STATUSMSG=Passed
		TABLECLASS=table-success
		;;
	    *FAILED*)
		STATUSMSG=Failed
		TABLECLASS=table-danger
		;;
	    *NOTRUN*)
		STATUSMSG="Not Run"
		;;
	    *"NOT STARTED"*)
		STATUSMSG="Not Started"
		;;
	esac

	echo "<tr class=$TABLECLASS>"
	echo "<td> $TESTNAME </td>"
	echo "<td> $STATUSMSG </td>"
	echo "<td> $DURATION </td>"
	echo "<td> <a href=$TESTNAME/log.br>	    log			</a> </td>"
	echo "<td> <a href=$TESTNAME/full_log.br>   full log		</a> </td>"
	echo "<td> <a href=$TESTNAME>		    output directory	</a> </td>"
	echo "</tr>"
    done

    echo "</table>"
    echo "</div>"
    echo "</body>"
    echo "</html>"
}

git_commit_html > $OUTPUT/index.html

git_log_html()
{
    echo '<!DOCTYPE HTML>'
    echo "<html><head><title>$BRANCH</title></head>"
    echo '<link href="bootstrap.min.css" rel="stylesheet">'

    echo '<body>'
    echo '<div class="container">'
    echo '<table class="table">'

    echo "<tr>"
    echo "<th> Commit      </th>"
    echo "<th> Description </th>"
    echo "<th> Passed      </th>"
    echo "<th> Failed      </th>"
    echo "<th> Not started </th>"
    echo "<th> Not run     </th>"
    echo "<th> Unknown     </th>"
    echo "<th> Total       </th>"
    echo "</tr>"

    git log --pretty=oneline $BRANCH|
	while read LINE; do
	    COMMIT=$(echo $LINE|cut -d\  -f1)
	    COMMIT_SHORT=$(echo $LINE|cut -b1-14)
	    DESCRIPTION=$(echo $LINE|cut -d\  -f2-)
	    RESULTS=$JOBSERVER_OUTPUT_DIR/c/$COMMIT

	    [[ ! -d $RESULTS ]] && break

	    if [[ -f $RESULTS/nr_tests ]]; then
		echo "<tr>"
		echo "<td> <a href=\"c/$COMMIT\">$COMMIT_SHORT</a> </td>"
		echo "<td> $DESCRIPTION </td>"
		echo "<td> $(<$RESULTS/nr_passed)      </td>"
		echo "<td> $(<$RESULTS/nr_failed)      </td>"
		echo "<td> $(<$RESULTS/nr_notstarted)  </td>"
		echo "<td> $(<$RESULTS/nr_notrun)      </td>"
		echo "<td> $(<$RESULTS/nr_unknown)     </td>"
		echo "<td> $(<$RESULTS/nr_tests)       </td>"
		echo "</tr>"
	    fi
	done

    echo "</table>"
    echo "</div>"
    echo "</body>"
    echo "</html>"
}

echo "Creating log for $BRANCH"
BRANCH_LOG=$(echo "$BRANCH"|tr / _).html
git_log_html > "$JOBSERVER_OUTPUT_DIR/$BRANCH_LOG"

echo "Success"
